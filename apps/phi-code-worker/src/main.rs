#![forbid(unsafe_code)]
use phi_ext_tools::{
    CapturedOutput,
    worker_protocol::{ExecutionError, JavaScriptRequest, JavaScriptResponse},
};
use rquickjs::{Context, Function, Runtime, Value, function::Func};
use std::{
    io::{self, Read, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn main() {
    let result = run();
    match result {
        Ok(response) => {
            if serde_json::to_writer(io::stdout().lock(), &response).is_err() {
                std::process::exit(1);
            }
        }
        Err(error) => {
            let _ = writeln!(io::stderr(), "{error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<JavaScriptResponse, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    io::stdin().take(2 * 1024 * 1024).read_to_end(&mut bytes)?;
    let request: JavaScriptRequest = serde_json::from_slice(&bytes)?;
    if request.code.len() > 256 * 1024
        || request.output_limit > 64 * 1024
        || request.result_limit > 64 * 1024
        || request.timeout_ms > 300_000
    {
        return Err("Worker request exceeds limits".into());
    }
    Ok(evaluate(&request)?)
}

fn evaluate(request: &JavaScriptRequest) -> Result<JavaScriptResponse, rquickjs::Error> {
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(256 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime)?;
    let stdout = Arc::new(Mutex::new(CapturedOutput::default()));
    let stderr = Arc::new(Mutex::new(CapturedOutput::default()));
    let mut response = context.with(|ctx| {
        let (out, err, limit) = (stdout.clone(), stderr.clone(), request.output_limit);
        ctx.globals().set(
            "__capture",
            Func::from(move |is_error: bool, text: String| {
                let mut output = if is_error { err.lock() } else { out.lock() }
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut keep = text.len().min(limit.saturating_sub(output.text.len()));
                while !text.is_char_boundary(keep) {
                    keep -= 1;
                }
                output.text.push_str(&text[..keep]);
                output.truncated |= keep < text.len();
            }),
        )?;
        // Closure owns pristine intrinsics: evaluated code cannot replace the encoder.
        let formatter: Function = ctx.eval(include_str!("runtime.js"))?;
        let completion = ctx.eval::<Value, _>(request.code.as_bytes());
        let mut response = JavaScriptResponse {
            stdout: CapturedOutput::default(),
            stderr: CapturedOutput::default(),
            value: None,
            error: None,
        };
        match completion {
            Ok(value) => match formatter.call::<_, String>((value, request.result_limit)) {
                Ok(encoded) if encoded.len() > request.result_limit => {
                    response.error = Some(ExecutionError {
                        kind: "ReturnEncodingError".into(),
                        message: "Encoded result exceeds its UTF-8 byte limit".into(),
                    });
                }
                Ok(encoded) => match serde_json::from_str(&encoded) {
                    Ok(value) => response.value = Some(value),
                    Err(error) => {
                        response.error = Some(ExecutionError {
                            kind: "ReturnEncodingError".into(),
                            message: error.to_string(),
                        });
                    }
                },
                Err(error) => {
                    response.error = Some(ExecutionError {
                        kind: "ReturnEncodingError".into(),
                        message: exception_message(&ctx, &error),
                    });
                }
            },
            Err(error) => {
                response.error = Some(ExecutionError {
                    kind: if Instant::now() >= deadline {
                        "Timeout"
                    } else {
                        "JavaScriptError"
                    }
                    .into(),
                    message: exception_message(&ctx, &error),
                });
            }
        }
        Ok::<_, rquickjs::Error>(response)
    })?;
    response.stdout = stdout
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    response.stderr = stderr
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(error) = &mut response.error
        && error.message.len() > request.output_limit
    {
        let mut keep = request.output_limit.saturating_sub(16);
        while !error.message.is_char_boundary(keep) {
            keep -= 1;
        }
        error.message.truncate(keep);
        error.message.push_str(" [truncated]");
    }
    Ok(response)
}

fn exception_message(ctx: &rquickjs::Ctx<'_>, error: &rquickjs::Error) -> String {
    if let rquickjs::Error::Exception = error {
        let caught = ctx.catch();
        if let Some(exception) = caught.as_exception() {
            return format!(
                "{}\n{}",
                exception.message().unwrap_or_default(),
                exception.stack().unwrap_or_default()
            );
        }
        if let Some(text) = caught.as_string() {
            return text
                .to_string()
                .unwrap_or_else(|_| "JavaScript threw an unreadable string".into());
        }
        format!("JavaScript threw a {} value", caught.type_of().as_str())
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn calculate(code: &str) -> JavaScriptResponse {
        evaluate(&JavaScriptRequest {
            code: code.into(),
            timeout_ms: 1000,
            output_limit: 1024,
            result_limit: 1024,
        })
        .unwrap()
    }
    #[test]
    fn calculates_and_preserves_exact_special_values() {
        assert_eq!(calculate("1 + 2").value, Some(serde_json::json!(3)));
        assert_eq!(
            calculate("9007199254740993n").value.unwrap()["value"],
            "9007199254740993"
        );
        assert!(
            calculate("({ nested: [undefined, 2n, NaN] })")
                .error
                .is_none()
        );
    }
    #[test]
    fn rejects_cycles_and_keeps_bounded_console_after_failure() {
        let result = calculate("console.log('x'.repeat(2000)); const x = {}; x.self = x; x");
        assert!(result.error.is_some());
        assert!(result.stdout.truncated);
        assert_eq!(result.stdout.text.len(), 1024);
        assert!(calculate("Promise.resolve(1)").error.is_some());
    }
    #[test]
    fn changed_intrinsics_do_not_silently_change_result_encoding() {
        assert_eq!(
            calculate("JSON.stringify = () => 'null'; ({answer: 42})").value,
            Some(serde_json::json!({"answer":42}))
        );
        assert_eq!(
            calculate("Object.prototype.toJSON = () => null; [1, undefined, 2n]").value,
            Some(serde_json::json!([1, {"kind":"undefined"}, {"kind":"bigint","value":"2"}]))
        );
        assert!(calculate("[1, , 3]").error.is_some());
        assert!(calculate("({[Symbol('hidden')]: 1})").error.is_some());
    }
}
