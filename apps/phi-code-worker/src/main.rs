#![forbid(unsafe_code)]
use phi_ext_tools::worker_protocol::{JavaScriptRequest, JavaScriptResponse};
use std::io::{self, Read, Write};

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
    Ok(phi_code_worker::JavaScriptEngine::default().evaluate(&request)?)
}
