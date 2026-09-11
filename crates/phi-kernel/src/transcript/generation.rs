use super::*;
use crate::{KernelError, ProviderContinuation};
use std::collections::{HashMap, HashSet};

impl TranscriptSession {
    pub fn ordered_rows(&self) -> Vec<TranscriptRow> {
        let mut rows = Vec::new();
        for row in self
            .rows
            .iter()
            .take_while(|row| !matches!(row.item, TurnItem::User { .. }))
        {
            if row.generation.is_none() {
                rows.push(row.clone());
            }
        }
        for (index, user) in self.rows.iter().enumerate() {
            if !matches!(user.item, TurnItem::User { .. }) {
                continue;
            }
            rows.push(user.clone());
            for row in self
                .rows
                .iter()
                .skip(index + 1)
                .take_while(|row| !matches!(row.item, TurnItem::User { .. }))
            {
                if row.generation.is_none() {
                    rows.push(row.clone());
                }
            }
            for row in &self.rows {
                if row
                    .generation
                    .as_ref()
                    .is_some_and(|stamp| stamp.user_message_id == user.id)
                {
                    rows.push(row.clone());
                }
            }
        }
        rows
    }
    pub fn history(&self) -> Result<Vec<TurnItem>> {
        match self
            .rows
            .iter()
            .rev()
            .find(|row| matches!(row.item, TurnItem::User { .. }))
        {
            Some(user) => self.history_through(&user.id),
            None => Ok(Vec::new()),
        }
    }
    pub fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut groups: HashMap<ModelResponseId, GenerationStamp> = HashMap::new();
        let mut calls: HashMap<(ModelResponseId, ToolCallId), (ToolName, bool)> = HashMap::new();
        let mut continuations = HashSet::new();
        for row in &self.rows {
            if !ids.insert(row.id.clone()) {
                return Err(KernelError::InvalidArgument(
                    "duplicate transcript message id".into(),
                ));
            }
            if matches!(row.item, TurnItem::ModelResponse { .. }) {
                return Err(KernelError::InvalidArgument(
                    "materialized response cannot be persisted as a row".into(),
                ));
            }
            if let Some(stamp) = &row.generation {
                if matches!(row.item, TurnItem::User { .. }) {
                    return Err(KernelError::InvalidArgument(
                        "user input cannot belong to model response".into(),
                    ));
                }
                if let Some(known) = groups.get(&stamp.response_id) {
                    if known != stamp {
                        return Err(KernelError::InvalidArgument(
                            "inconsistent model response grouping".into(),
                        ));
                    }
                } else {
                    groups.insert(stamp.response_id.clone(), stamp.clone());
                }
                match &row.item {
                    TurnItem::ToolCall {
                        tool_call_id,
                        tool_name,
                        ..
                    } => {
                        if !stamp.response_complete {
                            return Err(KernelError::InvalidArgument(
                                "incomplete response contains a tool call".into(),
                            ));
                        }
                        if calls
                            .insert(
                                (stamp.response_id.clone(), tool_call_id.clone()),
                                (tool_name.clone(), false),
                            )
                            .is_some()
                        {
                            return Err(KernelError::InvalidArgument(
                                "duplicate persisted tool call".into(),
                            ));
                        }
                    }
                    TurnItem::ToolResult {
                        tool_call_id,
                        tool_name,
                        ..
                    } => {
                        let Some((name, closed)) =
                            calls.get_mut(&(stamp.response_id.clone(), tool_call_id.clone()))
                        else {
                            return Err(KernelError::InvalidArgument(
                                "persisted tool result precedes or lacks its call".into(),
                            ));
                        };
                        if name != tool_name || *closed {
                            return Err(KernelError::InvalidArgument(
                                "mismatched or duplicate persisted tool result".into(),
                            ));
                        }
                        *closed = true;
                    }
                    TurnItem::Continuation { .. } => {
                        if !stamp.response_complete
                            || !continuations.insert(stamp.response_id.clone())
                        {
                            return Err(KernelError::InvalidArgument(
                                "invalid response continuation grouping".into(),
                            ));
                        }
                    }
                    _ => {}
                }
                if !self.rows.iter().any(|anchor| {
                    anchor.id == stamp.user_message_id
                        && matches!(anchor.item, TurnItem::User { .. })
                }) {
                    return Err(KernelError::InvalidArgument(
                        "generation user anchor is missing".into(),
                    ));
                }
                if !self.generations.iter().any(|record| {
                    record.job.job_id == stamp.job_id
                        && record.job.user_message_id == stamp.user_message_id
                }) {
                    return Err(KernelError::InvalidArgument(
                        "generation record is missing".into(),
                    ));
                }
            } else if matches!(
                row.item,
                TurnItem::ToolCall { .. }
                    | TurnItem::ToolResult { .. }
                    | TurnItem::Continuation { .. }
            ) {
                return Err(KernelError::InvalidArgument(
                    "tool or continuation row has no generation ownership".into(),
                ));
            }
        }
        let mut jobs = HashSet::new();
        for record in &self.generations {
            if !jobs.insert(record.job.job_id.clone()) {
                return Err(KernelError::InvalidArgument(
                    "duplicate generation id".into(),
                ));
            }
            if !self.rows.iter().any(|row| {
                row.id == record.job.user_message_id && matches!(row.item, TurnItem::User { .. })
            }) {
                return Err(KernelError::InvalidArgument(
                    "generation input not found".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn history_for(&self, job: &GenerationJob) -> Result<Vec<TurnItem>> {
        self.history_through(&job.user_message_id)
    }
    fn history_through(&self, user_message_id: &MessageId) -> Result<Vec<TurnItem>> {
        let anchor = self
            .rows
            .iter()
            .position(|row| row.id == *user_message_id && matches!(row.item, TurnItem::User { .. }))
            .ok_or_else(|| KernelError::InvalidArgument("generation input not found".into()))?;
        let mut history = Vec::new();
        for (index, user) in self.rows.iter().enumerate().take(anchor + 1) {
            if !matches!(user.item, TurnItem::User { .. }) {
                continue;
            }
            history.push(user.item.clone());
            // Legacy-free authored rows (e.g. imported text) stay beside their input.
            for row in self
                .rows
                .iter()
                .skip(index + 1)
                .take_while(|row| !matches!(row.item, TurnItem::User { .. }))
            {
                if row.generation.is_none() {
                    history.push(row.item.clone());
                }
            }
            let mut seen = HashSet::new();
            for row in &self.rows {
                let Some(stamp) = &row.generation else {
                    continue;
                };
                if stamp.user_message_id != user.id || !seen.insert(stamp.response_id.clone()) {
                    continue;
                }
                let mut rows = Vec::new();
                let mut continuation: Option<ProviderContinuation> = None;
                let mut results = Vec::new();
                for member in &self.rows {
                    if member.generation.as_ref().is_none_or(|g| {
                        g.response_id != stamp.response_id || g.job_id != stamp.job_id
                    }) {
                        continue;
                    }
                    match &member.item {
                        TurnItem::Continuation {
                            continuation: value,
                        } => continuation = Some(value.clone()),
                        TurnItem::ToolResult { .. } => results.push(member.item.clone()),
                        _ => rows.push(member.clone()),
                    }
                }
                history.push(TurnItem::ModelResponse {
                    response: ModelResponse {
                        id: stamp.response_id.clone(),
                        rows,
                        continuation,
                        complete: stamp.response_complete,
                    },
                });
                history.extend(results);
            }
        }
        Ok(history)
    }

    pub fn apply(&mut self, commit: &GenerationCommit) -> Result<()> {
        if !self.live {
            return Err(KernelError::SessionNotLive(
                commit.job().session_id.to_string(),
            ));
        }
        let job = commit.job();
        if !self
            .rows
            .iter()
            .any(|row| row.id == job.user_message_id && matches!(row.item, TurnItem::User { .. }))
        {
            return Err(KernelError::InvalidArgument(
                "generation input not found".into(),
            ));
        }
        match commit {
            GenerationCommit::Enqueue { .. } | GenerationCommit::Start { .. } => {
                let status = if matches!(commit, GenerationCommit::Enqueue { .. }) {
                    GenerationStatus::Pending
                } else {
                    GenerationStatus::Running
                };
                if let Some(record) = self
                    .generations
                    .iter_mut()
                    .find(|record| record.job.job_id == job.job_id)
                {
                    if !matches!(record.status, GenerationStatus::Pending) {
                        return Err(KernelError::InvalidArgument(
                            "generation already started".into(),
                        ));
                    }
                    record.status = status;
                } else {
                    self.generations.push(GenerationRecord {
                        job: job.clone(),
                        status,
                    });
                }
            }
            GenerationCommit::Response { response, .. } => {
                self.require_running(job)?;
                if self.rows.iter().any(|row| {
                    row.generation
                        .as_ref()
                        .is_some_and(|stamp| stamp.response_id == response.id)
                }) {
                    return Err(KernelError::InvalidArgument(
                        "duplicate model response id".into(),
                    ));
                }
                let mut calls = HashSet::new();
                for row in &response.rows {
                    match &row.item {
                        TurnItem::Assistant { .. } | TurnItem::Reasoning { .. } => {}
                        TurnItem::ToolCall { tool_call_id, .. } if response.complete => {
                            if !calls.insert(tool_call_id.clone()) {
                                return Err(KernelError::InvalidArgument(
                                    "duplicate tool call id".into(),
                                ));
                            }
                        }
                        _ => {
                            return Err(KernelError::InvalidArgument(
                                "invalid model response member".into(),
                            ));
                        }
                    }
                }
                let stamp = GenerationStamp {
                    job_id: job.job_id.clone(),
                    user_message_id: job.user_message_id.clone(),
                    response_id: response.id.clone(),
                    response_complete: response.complete,
                };
                for row in &response.rows {
                    let mut row = row.clone();
                    row.generation = Some(stamp.clone());
                    self.rows.push(row);
                }
                if let Some(continuation) = &response.continuation {
                    if !response.complete {
                        return Err(KernelError::InvalidArgument(
                            "incomplete response cannot carry continuation".into(),
                        ));
                    }
                    self.rows.push(TranscriptRow {
                        id: MessageId::generate(),
                        item: TurnItem::Continuation {
                            continuation: continuation.clone(),
                        },
                        generation: Some(stamp),
                    });
                }
            }
            GenerationCommit::ToolResult {
                response_id,
                tool_call_id,
                tool_name,
                output,
                status,
                ..
            } => {
                self.require_running(job)?;
                let members = || {
                    self.rows.iter().filter(|row| {
                        row.generation.as_ref().is_some_and(|stamp| {
                            stamp.job_id == job.job_id && stamp.response_id == *response_id
                        })
                    })
                };
                if !members().any(|row| matches!(&row.item, TurnItem::ToolCall { tool_call_id: id, tool_name: name, .. } if id == tool_call_id && name == tool_name)) { return Err(KernelError::InvalidArgument("tool result has no matching call".into())); }
                if members().any(|row| matches!(&row.item, TurnItem::ToolResult { tool_call_id: id, .. } if id == tool_call_id)) { return Err(KernelError::InvalidArgument("tool result already committed".into())); }
                self.rows.push(TranscriptRow {
                    id: MessageId::generate(),
                    item: TurnItem::ToolResult {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        output: output.clone(),
                        status: *status,
                    },
                    generation: Some(GenerationStamp {
                        job_id: job.job_id.clone(),
                        user_message_id: job.user_message_id.clone(),
                        response_id: response_id.clone(),
                        response_complete: true,
                    }),
                });
            }
            GenerationCommit::Finish { status, .. } => {
                if matches!(
                    status,
                    GenerationStatus::Pending | GenerationStatus::Running
                ) {
                    return Err(KernelError::InvalidArgument(
                        "finish requires terminal status".into(),
                    ));
                }
                let record = self
                    .generations
                    .iter_mut()
                    .find(|record| record.job.job_id == job.job_id)
                    .ok_or_else(|| KernelError::InvalidArgument("generation not found".into()))?;
                record.status = status.clone();
            }
        }
        self.validate()
    }

    fn require_running(&self, job: &GenerationJob) -> Result<()> {
        if self.generations.iter().any(|record| {
            record.job.job_id == job.job_id && record.status == GenerationStatus::Running
        }) {
            Ok(())
        } else {
            Err(KernelError::InvalidArgument(
                "generation is not running".into(),
            ))
        }
    }
}
