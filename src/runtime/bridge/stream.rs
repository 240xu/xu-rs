use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::ir::{
    BridgeError, CompletionIr, ContentIr, ResponseIr, ResponseMetaIr, SseFrame, StreamEventIr,
    UsageIr, WireProtocol,
};
use super::response::validate_response_ir;
use super::tools::{ToolCallTracker, DEFAULT_MAX_TOOL_ARGUMENT_BYTES};

pub const DEFAULT_MAX_SSE_FRAME_BYTES: usize = 256 * 1024;
pub const DEFAULT_MAX_STREAM_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_STREAM_ITEMS: usize = 256;

pub struct SseDecoder {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    frame_bytes: usize,
    max_frame_bytes: usize,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SSE_FRAME_BYTES)
    }
}

impl SseDecoder {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self {
            pending: Vec::new(),
            event: None,
            data: Vec::new(),
            frame_bytes: 0,
            max_frame_bytes,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, BridgeError> {
        let mut frames = Vec::new();
        for byte in bytes {
            self.pending.push(*byte);
            if self
                .frame_bytes
                .checked_add(self.pending.len())
                .is_none_or(|size| size > self.max_frame_bytes)
            {
                return Err(BridgeError::ResourceLimit);
            }
            if *byte != b'\n' {
                continue;
            }
            let line = std::mem::take(&mut self.pending);
            let line = &line[..line.len() - 1];
            self.frame_bytes = self
                .frame_bytes
                .checked_add(line.len() + 1)
                .ok_or(BridgeError::ResourceLimit)?;
            self.consume_line(line, &mut frames)?;
        }
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<Vec<SseFrame>, BridgeError> {
        if !self.pending.is_empty() || self.event.is_some() || !self.data.is_empty() {
            return Err(BridgeError::InvalidUpstream);
        }
        Ok(Vec::new())
    }

    fn consume_line(
        &mut self,
        raw_line: &[u8],
        frames: &mut Vec<SseFrame>,
    ) -> Result<(), BridgeError> {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() {
            if self.event.is_some() || !self.data.is_empty() {
                frames.push(SseFrame {
                    event: self.event.take(),
                    data: self.data.join("\n"),
                });
            }
            self.data.clear();
            self.frame_bytes = 0;
            return Ok(());
        }
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let line = std::str::from_utf8(line).map_err(|_| BridgeError::InvalidUpstream)?;
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct StreamState {
    protocol: Option<WireProtocol>,
    meta: ResponseMetaIr,
    started: bool,
    terminal: bool,
    failed: bool,
    pending_completion: Option<CompletionIr>,
    completion: Option<CompletionIr>,
    usage: Option<UsageIr>,
    content: Vec<ContentIr>,
    tools: ToolCallTracker,
    output_bytes: usize,
    max_output_bytes: usize,
    encoded_started: bool,
    encoded_text: bool,
    encoded_text_content: String,
    encoded_reasoning: bool,
    encoded_reasoning_summary: String,
    encoded_tools: BTreeSet<String>,
    encoded_tool_stops: BTreeSet<String>,
    encoded_terminal: bool,
    encoded_block_indexes: BTreeMap<String, usize>,
    encoded_item_ids: BTreeMap<String, String>,
    encoded_message_item_id: Option<String>,
    encoded_next_block_index: usize,
    encoded_message_id: Option<String>,
    encoded_reasoning_id: Option<String>,
    wire_tool_ids: BTreeMap<(u8, usize), String>,
    wire_tool_item_ids: BTreeMap<(u8, usize), String>,
    wire_item_ids: BTreeMap<(u8, usize), String>,
    wire_item_indexes: BTreeMap<String, (u8, usize)>,
    wire_item_statuses: BTreeMap<(u8, usize), String>,
    wire_block_kinds: BTreeMap<(u8, usize), u8>,
    wire_block_closed: BTreeSet<(u8, usize)>,
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamState {
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_MAX_TOOL_ARGUMENT_BYTES,
            DEFAULT_MAX_STREAM_OUTPUT_BYTES,
        )
    }

    pub fn with_limits(max_tool_argument_bytes: usize, max_output_bytes: usize) -> Self {
        Self {
            protocol: None,
            meta: ResponseMetaIr::default(),
            started: false,
            terminal: false,
            failed: false,
            pending_completion: None,
            completion: None,
            usage: None,
            content: Vec::new(),
            tools: ToolCallTracker::new(max_tool_argument_bytes),
            output_bytes: 0,
            max_output_bytes,
            encoded_started: false,
            encoded_text: false,
            encoded_text_content: String::new(),
            encoded_reasoning_summary: String::new(),
            encoded_reasoning: false,
            encoded_tools: BTreeSet::new(),
            encoded_tool_stops: BTreeSet::new(),
            encoded_terminal: false,
            encoded_block_indexes: BTreeMap::new(),
            encoded_item_ids: BTreeMap::new(),
            encoded_message_item_id: None,
            encoded_next_block_index: 0,
            encoded_message_id: None,
            encoded_reasoning_id: None,
            wire_tool_ids: BTreeMap::new(),
            wire_tool_item_ids: BTreeMap::new(),
            wire_item_ids: BTreeMap::new(),
            wire_item_indexes: BTreeMap::new(),
            wire_item_statuses: BTreeMap::new(),
            wire_block_kinds: BTreeMap::new(),
            wire_block_closed: BTreeSet::new(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub(crate) fn is_started(&self) -> bool {
        self.started
    }

    pub fn is_failed(&self) -> bool {
        self.failed
    }

    pub fn usage(&self) -> Option<&UsageIr> {
        self.usage.as_ref()
    }

    pub fn apply_event(&mut self, event: &StreamEventIr) -> Result<(), BridgeError> {
        let mut working = self.clone();
        working.apply_event_inner(event)?;
        *self = working;
        Ok(())
    }

    fn apply_event_inner(&mut self, event: &StreamEventIr) -> Result<(), BridgeError> {
        if self.terminal {
            return Err(BridgeError::ToolState);
        }
        match event {
            StreamEventIr::Started(meta) => self.start(meta),
            StreamEventIr::TextDelta { text } => {
                self.require_started()?;
                if self.content.len() >= DEFAULT_MAX_STREAM_ITEMS {
                    return Err(BridgeError::ResourceLimit);
                }
                self.push_output(text)?;
                self.content.push(ContentIr::Text(text.clone()));
                Ok(())
            }
            StreamEventIr::ReasoningDelta { text } => {
                self.require_started()?;
                if self.content.len() >= DEFAULT_MAX_STREAM_ITEMS {
                    return Err(BridgeError::ResourceLimit);
                }
                self.push_output(text)?;
                self.content.push(ContentIr::Thinking {
                    text: text.clone(),
                    signature: None,
                });
                Ok(())
            }
            StreamEventIr::ToolCallStarted(call) => {
                self.require_started()?;
                if !call.arguments.is_object() {
                    return Err(BridgeError::ToolState);
                }
                if self.tools.states().count() >= DEFAULT_MAX_STREAM_ITEMS {
                    return Err(BridgeError::ResourceLimit);
                }
                let arguments = (!call
                    .arguments
                    .as_object()
                    .is_some_and(|object| object.is_empty()))
                .then(|| serde_json::to_string(&call.arguments))
                .transpose()
                .map_err(|_| BridgeError::ToolState)?;
                self.tools.start(
                    &call.call_id,
                    call.item_id.as_deref(),
                    call.index,
                    &call.name,
                )?;
                if let Some(arguments) = arguments {
                    self.set_tool_arguments_if_empty(
                        &call.call_id,
                        call.item_id.as_deref(),
                        &arguments,
                    )?;
                }
                Ok(())
            }
            StreamEventIr::ToolCallArgumentsDelta { call_id, delta } => {
                self.require_started()?;
                let item_id = self.tool_state(call_id)?.item_id().map(str::to_owned);
                let mut tools = self.tools.clone();
                tools.push_arguments(call_id, item_id.as_deref(), delta)?;
                self.charge_output_bytes(delta.len())?;
                self.tools = tools;
                Ok(())
            }
            StreamEventIr::ToolCallFinished { call_id } => {
                self.require_started()?;
                let item_id = self.tool_state(call_id)?.item_id().map(str::to_owned);
                let arguments = self
                    .tools
                    .get(call_id, item_id.as_deref())?
                    .arguments()
                    .to_string();
                let parsed: Value =
                    serde_json::from_str(&arguments).map_err(|_| BridgeError::ToolState)?;
                if !parsed.is_object() {
                    return Err(BridgeError::ToolState);
                }
                self.tools.finish(call_id, item_id.as_deref())
            }
            StreamEventIr::Usage(usage) => {
                self.require_started()?;
                validate_usage(usage)?;
                self.usage = Some(merge_usage(self.usage.take(), usage)?);
                Ok(())
            }
            StreamEventIr::Completed(completion) => self.complete(completion),
            StreamEventIr::Failed(_) => {
                self.failed = true;
                self.terminal = true;
                Ok(())
            }
        }
    }

    pub fn finish_eof(&self) -> Result<(), BridgeError> {
        self.terminal
            .then_some(())
            .ok_or(BridgeError::InvalidUpstream)
    }

    pub(crate) fn bind_protocol(&mut self, protocol: WireProtocol) -> Result<(), BridgeError> {
        if let Some(bound) = self.protocol {
            if bound != protocol {
                return Err(BridgeError::InvalidRequest);
            }
        } else {
            self.protocol = Some(protocol);
        }
        Ok(())
    }

    pub(crate) fn next_tool_index(&self) -> usize {
        self.tools.states().count()
    }

    pub(crate) fn next_wire_block_index(&self, protocol: WireProtocol) -> usize {
        let protocol = protocol_number(protocol);
        self.wire_block_kinds
            .keys()
            .filter(|(entry_protocol, _)| *entry_protocol == protocol)
            .count()
    }

    pub(crate) fn require_next_output_index(&self, index: usize) -> Result<(), BridgeError> {
        if index != self.next_wire_block_index(WireProtocol::OpenAiResponses) {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn require_response_identity(
        &self,
        id: &str,
        model: &str,
    ) -> Result<(), BridgeError> {
        if self.meta.id.as_deref() != Some(id) || self.meta.model.as_deref() != Some(model) {
            return Err(BridgeError::InvalidUpstream);
        }
        Ok(())
    }

    pub(crate) fn require_response_id(&self, id: &str) -> Result<(), BridgeError> {
        if self.meta.id.as_deref() != Some(id) {
            return Err(BridgeError::InvalidUpstream);
        }
        Ok(())
    }

    pub(crate) fn register_wire_block(
        &mut self,
        protocol: WireProtocol,
        index: usize,
        kind: u8,
    ) -> Result<(), BridgeError> {
        let key = (protocol_number(protocol), index);
        if !self.wire_block_kinds.contains_key(&key)
            && self.wire_block_kinds.len() >= DEFAULT_MAX_STREAM_ITEMS
        {
            return Err(BridgeError::ResourceLimit);
        }
        if self.wire_block_kinds.insert(key, kind).is_some() {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn register_wire_item(
        &mut self,
        protocol: WireProtocol,
        index: usize,
        item_id: &str,
        status: &str,
    ) -> Result<(), BridgeError> {
        if item_id.trim().is_empty() || status != "in_progress" {
            return Err(BridgeError::ToolState);
        }
        let key = (protocol_number(protocol), index);
        if self.wire_item_ids.contains_key(&key)
            || self.wire_item_statuses.contains_key(&key)
            || self.wire_item_indexes.contains_key(item_id)
        {
            return Err(BridgeError::ToolState);
        }
        self.wire_item_ids.insert(key, item_id.to_string());
        self.wire_item_indexes.insert(item_id.to_string(), key);
        self.wire_item_statuses.insert(key, status.to_string());
        Ok(())
    }

    pub(crate) fn require_wire_item(
        &self,
        protocol: WireProtocol,
        index: usize,
        item_id: &str,
    ) -> Result<(), BridgeError> {
        if self
            .wire_item_ids
            .get(&(protocol_number(protocol), index))
            .is_none_or(|expected| expected != item_id)
        {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn transition_wire_item(
        &mut self,
        protocol: WireProtocol,
        index: usize,
        expected: &str,
        next: &str,
    ) -> Result<(), BridgeError> {
        let status = self
            .wire_item_statuses
            .get_mut(&(protocol_number(protocol), index))
            .ok_or(BridgeError::ToolState)?;
        if status != expected {
            return Err(BridgeError::ToolState);
        }
        *status = next.to_string();
        Ok(())
    }

    pub(crate) fn wire_block_kind(
        &self,
        protocol: WireProtocol,
        index: usize,
    ) -> Result<u8, BridgeError> {
        self.wire_block_kinds
            .get(&(protocol_number(protocol), index))
            .copied()
            .ok_or(BridgeError::ToolState)
    }

    pub(crate) fn require_wire_block_kind(
        &self,
        protocol: WireProtocol,
        index: usize,
        expected: u8,
    ) -> Result<(), BridgeError> {
        if self.wire_block_kind(protocol, index)? != expected {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn require_wire_block_open(
        &self,
        protocol: WireProtocol,
        index: usize,
    ) -> Result<(), BridgeError> {
        let key = (protocol_number(protocol), index);
        if !self.wire_block_kinds.contains_key(&key) || self.wire_block_closed.contains(&key) {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn close_wire_block(
        &mut self,
        protocol: WireProtocol,
        index: usize,
    ) -> Result<(), BridgeError> {
        let key = (protocol_number(protocol), index);
        if !self.wire_block_kinds.contains_key(&key) || !self.wire_block_closed.insert(key) {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn require_wire_blocks_closed(
        &self,
        protocol: WireProtocol,
    ) -> Result<(), BridgeError> {
        let protocol = protocol_number(protocol);
        if self
            .wire_block_kinds
            .keys()
            .filter(|(entry_protocol, _)| *entry_protocol == protocol)
            .any(|key| !self.wire_block_closed.contains(key))
        {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn finish_open_tools(&mut self) -> Result<Vec<StreamEventIr>, BridgeError> {
        let call_ids: Vec<String> = self
            .tools
            .states()
            .filter(|tool| !tool.is_completed())
            .map(|tool| tool.call_id().to_string())
            .collect();
        let mut events = Vec::with_capacity(call_ids.len());
        for call_id in call_ids {
            let item_id = self.tool_state(&call_id)?.item_id().map(str::to_owned);
            if self
                .tools
                .get(&call_id, item_id.as_deref())?
                .arguments()
                .is_empty()
            {
                self.set_tool_arguments_if_empty(&call_id, item_id.as_deref(), "{}")?;
            }
            let event = StreamEventIr::ToolCallFinished { call_id };
            self.apply_event(&event)?;
            events.push(event);
        }
        Ok(events)
    }

    pub(crate) fn set_tool_arguments_final(
        &mut self,
        call_id: &str,
        arguments: &str,
    ) -> Result<(), BridgeError> {
        let item_id = self.tool_state(call_id)?.item_id().map(str::to_owned);
        let current = self
            .tools
            .get(call_id, item_id.as_deref())?
            .arguments()
            .to_string();
        let additional = if current == arguments {
            0
        } else if arguments.starts_with(&current) {
            arguments.len() - current.len()
        } else {
            arguments.len()
        };
        let mut tools = self.tools.clone();
        tools.set_arguments_final(call_id, item_id.as_deref(), arguments)?;
        self.charge_output_bytes(additional)?;
        self.tools = tools;
        Ok(())
    }

    fn set_tool_arguments_if_empty(
        &mut self,
        call_id: &str,
        item_id: Option<&str>,
        arguments: &str,
    ) -> Result<(), BridgeError> {
        let current = self.tools.get(call_id, item_id)?.arguments().to_string();
        let mut tools = self.tools.clone();
        tools.set_arguments_if_empty(call_id, item_id, arguments)?;
        let additional = if current.is_empty() {
            arguments.len()
        } else {
            0
        };
        self.charge_output_bytes(additional)?;
        self.tools = tools;
        Ok(())
    }

    pub(crate) fn set_pending_completion(
        &mut self,
        completion: CompletionIr,
    ) -> Result<(), BridgeError> {
        self.require_started()?;
        if self.pending_completion.is_some() || self.terminal {
            return Err(BridgeError::ToolState);
        }
        if completion.error.is_some() || completion.status.as_deref() == Some("failed") {
            return Err(BridgeError::InvalidUpstream);
        }
        self.pending_completion = Some(completion);
        Ok(())
    }

    pub(crate) fn require_no_pending_completion(&self) -> Result<(), BridgeError> {
        if self.pending_completion.is_some() {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn take_pending_completion(&mut self) -> Result<CompletionIr, BridgeError> {
        self.pending_completion
            .take()
            .ok_or(BridgeError::InvalidUpstream)
    }

    pub(crate) fn meta(&self) -> &ResponseMetaIr {
        &self.meta
    }

    pub(crate) fn tool_state(
        &self,
        call_id: &str,
    ) -> Result<&super::tools::ToolCallState, BridgeError> {
        self.tools
            .states()
            .find(|state| state.call_id() == call_id)
            .ok_or(BridgeError::ToolState)
    }

    pub(crate) fn register_wire_tool(
        &mut self,
        protocol: WireProtocol,
        index: usize,
        call_id: &str,
    ) -> Result<(), BridgeError> {
        let key = (protocol_number(protocol), index);
        if self
            .wire_tool_ids
            .insert(key, call_id.to_string())
            .is_some()
        {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn register_wire_tool_with_item(
        &mut self,
        protocol: WireProtocol,
        index: usize,
        call_id: &str,
        item_id: &str,
    ) -> Result<(), BridgeError> {
        self.register_wire_tool(protocol, index, call_id)?;
        if item_id.trim().is_empty()
            || self
                .wire_tool_item_ids
                .insert((protocol_number(protocol), index), item_id.to_string())
                .is_some()
        {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn require_wire_tool_item(
        &self,
        protocol: WireProtocol,
        index: usize,
        item_id: &str,
    ) -> Result<(), BridgeError> {
        if self
            .wire_tool_item_ids
            .get(&(protocol_number(protocol), index))
            .is_none_or(|expected| expected != item_id)
        {
            return Err(BridgeError::ToolState);
        }
        Ok(())
    }

    pub(crate) fn wire_tool_call_id(
        &self,
        protocol: WireProtocol,
        index: usize,
    ) -> Result<String, BridgeError> {
        self.wire_tool_ids
            .get(&(protocol_number(protocol), index))
            .cloned()
            .ok_or(BridgeError::ToolState)
    }

    pub(crate) fn tool_call_id_for_item(&self, item_id: &str) -> Result<String, BridgeError> {
        self.tools
            .states()
            .find(|state| state.item_id() == Some(item_id))
            .map(|state| state.call_id().to_string())
            .ok_or(BridgeError::ToolState)
    }

    pub(crate) fn encoded_started(&self) -> bool {
        self.encoded_started
    }

    pub(crate) fn mark_encoded_started(&mut self) {
        self.encoded_started = true;
    }

    pub(crate) fn encoded_text(&self) -> bool {
        self.encoded_text
    }

    pub(crate) fn mark_encoded_text(&mut self) {
        self.encoded_text = true;
    }

    pub(crate) fn append_encoded_text(&mut self, text: &str) {
        self.encoded_text_content.push_str(text);
    }

    pub(crate) fn encoded_text_content(&self) -> &str {
        &self.encoded_text_content
    }

    pub(crate) fn append_encoded_reasoning_summary(&mut self, text: &str) {
        self.encoded_reasoning_summary.push_str(text);
    }

    pub(crate) fn encoded_reasoning_summary(&self) -> &str {
        &self.encoded_reasoning_summary
    }

    pub(crate) fn encoded_reasoning(&self) -> bool {
        self.encoded_reasoning
    }

    pub(crate) fn mark_encoded_reasoning(&mut self) {
        self.encoded_reasoning = true;
    }

    pub(crate) fn encoded_tool(&self, call_id: &str) -> bool {
        self.encoded_tools.contains(call_id)
    }

    pub(crate) fn mark_encoded_tool(&mut self, call_id: &str) {
        self.encoded_tools.insert(call_id.to_string());
    }

    pub(crate) fn encoded_tool_stop(&self, call_id: &str) -> bool {
        self.encoded_tool_stops.contains(call_id)
    }

    pub(crate) fn mark_encoded_tool_stop(&mut self, call_id: &str) {
        self.encoded_tool_stops.insert(call_id.to_string());
    }

    pub(crate) fn encoded_terminal(&self) -> bool {
        self.encoded_terminal
    }

    pub(crate) fn mark_encoded_terminal(&mut self) {
        self.encoded_terminal = true;
    }

    pub(crate) fn encoded_block_index(&mut self, key: &str) -> usize {
        if let Some(index) = self.encoded_block_indexes.get(key) {
            return *index;
        }
        let index = self.encoded_next_block_index;
        self.encoded_next_block_index += 1;
        self.encoded_block_indexes.insert(key.to_string(), index);
        index
    }

    pub(crate) fn encoded_message_id(&mut self) -> String {
        self.encoded_message_id
            .get_or_insert_with(|| super::response::generated_id("msg"))
            .clone()
    }

    pub(crate) fn encoded_reasoning_id(&mut self) -> String {
        self.encoded_reasoning_id
            .get_or_insert_with(|| super::response::generated_id("rs"))
            .clone()
    }

    pub(crate) fn encoded_item_id(&mut self, call_id: &str, item_id: Option<&str>) -> String {
        if let Some(existing) = self.encoded_item_ids.get(call_id) {
            return existing.clone();
        }
        let value = item_id
            .filter(|item_id| !item_id.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("fc_{call_id}"));
        self.encoded_item_ids
            .insert(call_id.to_string(), value.clone());
        value
    }

    pub(crate) fn encoded_message_item_id(&mut self) -> String {
        self.encoded_message_item_id
            .get_or_insert_with(|| super::response::generated_id("msg_item"))
            .clone()
    }

    fn start(&mut self, meta: &ResponseMetaIr) -> Result<(), BridgeError> {
        if self.started
            || meta.id.as_deref().is_none_or(|id| id.trim().is_empty())
            || meta
                .model
                .as_deref()
                .is_none_or(|model| model.trim().is_empty())
        {
            return Err(BridgeError::ToolState);
        }
        self.meta = meta.clone();
        self.started = true;
        Ok(())
    }

    fn complete(&mut self, completion: &CompletionIr) -> Result<(), BridgeError> {
        self.require_started()?;
        if self.pending_completion.is_some() {
            return Err(BridgeError::ToolState);
        }
        for tool in self.tools.states() {
            if !tool.is_completed() {
                return Err(BridgeError::ToolState);
            }
            let parsed: Value =
                serde_json::from_str(tool.arguments()).map_err(|_| BridgeError::ToolState)?;
            if !parsed.is_object() {
                return Err(BridgeError::ToolState);
            }
        }
        let response = ResponseIr {
            meta: self.meta.clone(),
            content: self.terminal_content()?,
            usage: self.usage.clone(),
            completion: completion.clone(),
            extensions: Default::default(),
        };
        validate_response_ir(&response)?;
        self.completion = Some(completion.clone());
        self.terminal = true;
        Ok(())
    }

    fn terminal_content(&self) -> Result<Vec<ContentIr>, BridgeError> {
        let mut content = self.content.clone();
        for tool in self.tools.states() {
            let arguments: Value =
                serde_json::from_str(tool.arguments()).map_err(|_| BridgeError::ToolState)?;
            content.push(ContentIr::ToolUse(super::ir::ToolCallIr {
                call_id: tool.call_id().to_string(),
                item_id: tool.item_id().map(str::to_owned),
                index: tool.index(),
                name: tool.name().to_string(),
                arguments,
            }));
        }
        Ok(content)
    }

    fn require_started(&self) -> Result<(), BridgeError> {
        self.started.then_some(()).ok_or(BridgeError::ToolState)
    }

    fn push_output(&mut self, text: &str) -> Result<(), BridgeError> {
        self.charge_output_bytes(text.len())
    }

    fn charge_output_bytes(&mut self, bytes: usize) -> Result<(), BridgeError> {
        self.output_bytes = self
            .output_bytes
            .checked_add(bytes)
            .ok_or(BridgeError::ResourceLimit)?;
        if self.output_bytes > self.max_output_bytes {
            return Err(BridgeError::ResourceLimit);
        }
        Ok(())
    }
}

fn protocol_number(protocol: WireProtocol) -> u8 {
    match protocol {
        WireProtocol::AnthropicMessages => 0,
        WireProtocol::OpenAiChat => 1,
        WireProtocol::OpenAiResponses => 2,
    }
}

pub fn decode_stream_frame(
    protocol: WireProtocol,
    frame: &SseFrame,
    state: &mut StreamState,
) -> Result<Vec<StreamEventIr>, BridgeError> {
    let mut working = state.clone();
    working.bind_protocol(protocol)?;
    if frame.data == "[DONE]" {
        if protocol != WireProtocol::OpenAiChat {
            return Err(BridgeError::InvalidUpstream);
        }
        let mut events = working.finish_open_tools()?;
        let completion = working.take_pending_completion()?;
        working.apply_event(&StreamEventIr::Completed(completion.clone()))?;
        events.push(StreamEventIr::Completed(completion.clone()));
        *state = working;
        return Ok(events);
    }
    if frame.data.trim().is_empty() {
        return Err(BridgeError::InvalidUpstream);
    }
    let events = match protocol {
        WireProtocol::AnthropicMessages => {
            super::anthropic::decode_stream_frame(frame, &mut working)
        }
        WireProtocol::OpenAiChat => super::chat::decode_stream_frame(frame, &mut working),
        WireProtocol::OpenAiResponses => super::responses::decode_stream_frame(frame, &mut working),
    }?;
    *state = working;
    Ok(events)
}

pub fn encode_stream_events(
    protocol: WireProtocol,
    events: &[StreamEventIr],
    state: &mut StreamState,
) -> Result<Vec<SseFrame>, BridgeError> {
    let mut working = state.clone();
    working.bind_protocol(protocol)?;
    let mut frames = Vec::new();
    for event in events {
        working.apply_event(event)?;
        frames.extend(match protocol {
            WireProtocol::AnthropicMessages => {
                super::anthropic::encode_stream_event(event, &mut working)?
            }
            WireProtocol::OpenAiChat => super::chat::encode_stream_event(event, &mut working)?,
            WireProtocol::OpenAiResponses => {
                super::responses::encode_stream_event(event, &mut working)?
            }
        });
    }
    *state = working;
    Ok(frames)
}

fn validate_usage(usage: &UsageIr) -> Result<(), BridgeError> {
    if let (Some(input), Some(output), Some(total)) =
        (usage.input_tokens, usage.output_tokens, usage.total_tokens)
    {
        if input.checked_add(output) != Some(total) {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn merge_usage(current: Option<UsageIr>, next: &UsageIr) -> Result<UsageIr, BridgeError> {
    let current = current.unwrap_or_default();
    let merged = UsageIr {
        input_tokens: next.input_tokens.or(current.input_tokens),
        output_tokens: next.output_tokens.or(current.output_tokens),
        total_tokens: next.total_tokens.or(current.total_tokens),
        cache_read_tokens: next.cache_read_tokens.or(current.cache_read_tokens),
        cache_write_tokens: next.cache_write_tokens.or(current.cache_write_tokens),
        reasoning_tokens: next.reasoning_tokens.or(current.reasoning_tokens),
    };
    validate_usage(&merged)?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::ir::{
        BridgeError, CompletionIr, ResponseMetaIr, SseFrame, StreamEventIr, ToolCallIr, UsageIr,
        WireProtocol,
    };
    use super::super::{decode_stream_frame, encode_stream_events, SseDecoder, StreamState};
    use super::DEFAULT_MAX_STREAM_ITEMS;

    fn frame(event: Option<&str>, data: &str) -> SseFrame {
        SseFrame {
            event: event.map(str::to_owned),
            data: data.to_string(),
        }
    }

    fn started(id: &str, model: &str) -> StreamEventIr {
        StreamEventIr::Started(ResponseMetaIr {
            id: Some(id.to_string()),
            model: Some(model.to_string()),
        })
    }

    fn tool(call_id: &str, item_id: Option<&str>, index: usize, name: &str) -> StreamEventIr {
        StreamEventIr::ToolCallStarted(ToolCallIr {
            call_id: call_id.to_string(),
            item_id: item_id.map(str::to_owned),
            index,
            name: name.to_string(),
            arguments: json!({}),
        })
    }

    fn tool_with_arguments(
        call_id: &str,
        item_id: Option<&str>,
        index: usize,
        name: &str,
    ) -> StreamEventIr {
        StreamEventIr::ToolCallStarted(ToolCallIr {
            call_id: call_id.to_string(),
            item_id: item_id.map(str::to_owned),
            index,
            name: name.to_string(),
            arguments: json!({ "seed": 1 }),
        })
    }

    fn completed(finish_reason: &str, stop_reason: &str) -> StreamEventIr {
        StreamEventIr::Completed(CompletionIr {
            finish_reason: Some(finish_reason.to_string()),
            stop_reason: Some(stop_reason.to_string()),
            status: Some("completed".to_string()),
            error: None,
        })
    }

    fn event_data(frame: &SseFrame) -> Value {
        serde_json::from_str(&frame.data).expect("encoded frame data is JSON")
    }

    #[test]
    fn sse_decoder_handles_crlf_multiline_data_comments_and_empty_frames() {
        let mut decoder = SseDecoder::new(256);

        let frames = decoder
            .feed(
                b": keep-alive\r\n\r\nevent: message\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n\r\ndata: [DONE]\r\n\r\n",
            )
            .expect("valid SSE input decodes");

        assert_eq!(
            frames,
            vec![frame(Some("message"), "{\"a\":\n1}"), frame(None, "[DONE]"),]
        );
        assert!(decoder
            .finish()
            .expect("clean EOF has no partial frame")
            .is_empty());
    }

    #[test]
    fn sse_decoder_rejects_overlong_frames_and_partial_eof() {
        let mut decoder = SseDecoder::new(8);
        assert_eq!(
            decoder.feed(b"data: 12345\n\n"),
            Err(BridgeError::ResourceLimit)
        );

        let mut decoder = SseDecoder::new(64);
        decoder
            .feed(b"data: {\"partial\": true}")
            .expect("line is buffered");
        assert_eq!(decoder.finish(), Err(BridgeError::InvalidUpstream));
    }

    #[test]
    fn sse_frame_limit_applies_per_frame_not_per_feed_batch() {
        let mut decoder = SseDecoder::new(16);
        let frames = decoder
            .feed(b"data: first\n\ndata: second\n\n")
            .expect("multiple bounded frames share one feed");
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn malformed_json_is_rejected_before_entering_stream_state() {
        let mut state = StreamState::new();

        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiChat,
                &frame(None, "{not-json"),
                &mut state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
    }

    #[test]
    fn chat_done_is_the_terminal_event_and_finish_chunk_only_sets_completion_metadata() {
        let mut state = StreamState::new();
        let frames = [
            frame(
                None,
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"hi"},"finish_reason":null}]}"#,
            ),
            frame(
                None,
                r#"{"id":"chat-1","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            ),
            frame(None, "[DONE]"),
        ];

        let mut events = Vec::new();
        for frame in &frames {
            events.extend(
                decode_stream_frame(WireProtocol::OpenAiChat, frame, &mut state)
                    .expect("Chat stream frame decodes"),
            );
        }

        assert!(matches!(events.first(), Some(StreamEventIr::Started(_))));
        assert!(events
            .iter()
            .any(|event| { matches!(event, StreamEventIr::TextDelta { text } if text == "hi") }));
        assert!(matches!(events.last(), Some(StreamEventIr::Completed(_))));
        assert!(state.is_terminal());
    }

    #[test]
    fn stream_state_accepts_reasoning_tools_usage_and_terminal_in_order() {
        let mut state = StreamState::new();
        let events = [
            started("resp-1", "model"),
            StreamEventIr::ReasoningDelta {
                text: "plan".to_string(),
            },
            tool("call-1", Some("item-1"), 0, "lookup"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: r#"{"q":"rust"}"#.to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            StreamEventIr::Usage(UsageIr {
                input_tokens: Some(2),
                output_tokens: Some(3),
                total_tokens: Some(5),
                ..UsageIr::default()
            }),
            completed("tool_calls", "tool_use"),
        ];

        for event in &events {
            state.apply_event(event).expect("event order is valid");
        }

        assert!(state.is_terminal());
        assert_eq!(state.usage().and_then(|usage| usage.total_tokens), Some(5));
    }

    #[test]
    fn stream_state_rejects_invalid_lifecycle_and_unfinished_tool_json() {
        let mut state = StreamState::new();
        assert_eq!(
            state.apply_event(&StreamEventIr::TextDelta {
                text: "early".to_string(),
            }),
            Err(BridgeError::ToolState)
        );

        state
            .apply_event(&started("resp-1", "model"))
            .expect("start accepted");
        state
            .apply_event(&tool("call-1", None, 0, "lookup"))
            .expect("tool start accepted");
        assert_eq!(
            state.apply_event(&tool("call-1", None, 1, "duplicate")),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            state.apply_event(&StreamEventIr::ToolCallArgumentsDelta {
                call_id: "unknown".to_string(),
                delta: "{}".to_string(),
            }),
            Err(BridgeError::ToolState)
        );

        state
            .apply_event(&StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: "{broken".to_string(),
            })
            .expect("argument fragment is bounded and buffered");
        assert_eq!(
            state.apply_event(&completed("tool_calls", "tool_use")),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn stream_state_apply_event_is_atomic_on_validation_failure() {
        let mut state = StreamState::new();
        state
            .apply_event(&started("resp-1", "model"))
            .expect("stream starts");
        state
            .apply_event(&StreamEventIr::Usage(UsageIr {
                input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3),
                ..UsageIr::default()
            }))
            .expect("initial usage is valid");
        assert_eq!(
            state.apply_event(&StreamEventIr::Usage(UsageIr {
                total_tokens: Some(4),
                ..UsageIr::default()
            })),
            Err(BridgeError::InvalidUpstream)
        );
        assert_eq!(state.usage().and_then(|usage| usage.total_tokens), Some(3));

        let mut bounded = StreamState::with_limits(4, super::DEFAULT_MAX_STREAM_OUTPUT_BYTES);
        bounded
            .apply_event(&started("resp-2", "model"))
            .expect("bounded stream starts");
        assert_eq!(
            bounded.apply_event(&StreamEventIr::ToolCallStarted(ToolCallIr {
                call_id: "call-1".to_string(),
                item_id: None,
                index: 0,
                name: "lookup".to_string(),
                arguments: json!({ "seed": 1 }),
            })),
            Err(BridgeError::ResourceLimit)
        );
        assert!(bounded.tool_state("call-1").is_err());
    }

    #[test]
    fn stream_state_rejects_duplicate_finish_and_events_after_terminal() {
        let mut state = StreamState::new();
        state
            .apply_event(&started("resp-1", "model"))
            .expect("start accepted");
        state
            .apply_event(&StreamEventIr::TextDelta {
                text: "answer".to_string(),
            })
            .expect("text accepted");
        state
            .apply_event(&completed("stop", "end_turn"))
            .expect("completion accepted");
        assert_eq!(
            state.apply_event(&completed("stop", "end_turn")),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            state.apply_event(&StreamEventIr::TextDelta {
                text: "late".to_string(),
            }),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn anthropic_encoder_emits_message_and_tool_block_lifecycles_once() {
        let events = [
            started("msg-1", "model"),
            StreamEventIr::TextDelta {
                text: "answer".to_string(),
            },
            tool("call-1", None, 0, "lookup"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: "{}".to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            StreamEventIr::Usage(UsageIr {
                input_tokens: Some(1),
                output_tokens: Some(2),
                ..UsageIr::default()
            }),
            completed("tool_calls", "tool_use"),
        ];
        let mut state = StreamState::new();

        let output = encode_stream_events(WireProtocol::AnthropicMessages, &events, &mut state)
            .expect("Anthropic stream encodes");
        let types: Vec<String> = output
            .iter()
            .filter_map(|frame| {
                event_data(frame)
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();

        assert_eq!(types.first().map(String::as_str), Some("message_start"));
        assert_eq!(
            types
                .iter()
                .filter(|kind| kind.as_str() == "message_start")
                .count(),
            1
        );
        assert_eq!(
            types
                .iter()
                .filter(|kind| kind.as_str() == "content_block_start")
                .count(),
            2
        );
        assert_eq!(
            types
                .iter()
                .filter(|kind| kind.as_str() == "content_block_stop")
                .count(),
            2
        );
        assert_eq!(types.last().map(String::as_str), Some("message_stop"));
        assert_eq!(
            output[output.len() - 1].event.as_deref(),
            Some("message_stop")
        );
    }

    #[test]
    fn chat_encoder_keeps_parallel_tool_indexes_and_ids_stable() {
        let events = [
            started("chat-1", "model"),
            tool("call-1", None, 0, "one"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: "{}".to_string(),
            },
            tool("call-2", None, 1, "two"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-2".to_string(),
                delta: "{}".to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-2".to_string(),
            },
            completed("tool_calls", "tool_use"),
        ];
        let mut state = StreamState::new();

        let output = encode_stream_events(WireProtocol::OpenAiChat, &events, &mut state)
            .expect("Chat stream encodes");
        let tool_chunks: Vec<_> = output
            .iter()
            .filter(|frame| frame.data != "[DONE]")
            .filter_map(|frame| {
                let payload = event_data(frame);
                let tool = payload.pointer("/choices/0/delta/tool_calls/0")?;
                tool.get("id").is_some().then(|| tool.clone())
            })
            .collect();

        assert_eq!(tool_chunks.len(), 2);
        assert_eq!(tool_chunks[0]["index"], json!(0));
        assert_eq!(tool_chunks[0]["id"], json!("call-1"));
        assert_eq!(tool_chunks[1]["index"], json!(1));
        assert_eq!(tool_chunks[1]["id"], json!("call-2"));
        assert_eq!(
            output.last().map(|frame| frame.data.as_str()),
            Some("[DONE]")
        );
    }

    #[test]
    fn responses_encoder_separates_output_item_ids_from_call_ids() {
        let events = [
            started("resp-1", "model"),
            StreamEventIr::TextDelta {
                text: "answer".to_string(),
            },
            tool("call-1", Some("item-1"), 0, "lookup"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: "{}".to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            completed("tool_calls", "tool_use"),
        ];
        let mut state = StreamState::new();

        let output = encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut state)
            .expect("Responses stream encodes");
        let added = output
            .iter()
            .find(|frame| {
                frame.event.as_deref() == Some("response.output_item.added")
                    && event_data(frame)["item"]["type"] == json!("function_call")
            })
            .map(event_data)
            .expect("function output item is present");
        assert_eq!(added["item"]["id"], json!("item-1"));
        assert_eq!(added["item"]["call_id"], json!("call-1"));
        assert!(output
            .iter()
            .any(|frame| frame.event.as_deref() == Some("response.function_call_arguments.delta")));
        let argument_done = output
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.function_call_arguments.done"))
            .map(event_data)
            .expect("Responses tool arguments finalize");
        assert_eq!(argument_done["item_id"], json!("item-1"));
        assert_eq!(argument_done["output_index"], json!(1));
        assert_eq!(argument_done["arguments"], json!("{}"));
        assert_eq!(
            output.last().and_then(|frame| frame.event.as_deref()),
            Some("response.completed")
        );
    }

    #[test]
    fn responses_encoder_tool_done_item_includes_name_and_full_arguments() {
        let events = [
            started("resp-1", "model"),
            tool("call-1", Some("item-1"), 0, "lookup"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: r#"{"q":"rust"}"#.to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            completed("tool_calls", "tool_use"),
        ];
        let mut state = StreamState::new();

        let output = encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut state)
            .expect("Responses stream encodes");
        let done = output
            .iter()
            .find(|frame| {
                frame.event.as_deref() == Some("response.output_item.done")
                    && event_data(frame)["item"]["type"] == json!("function_call")
            })
            .map(event_data)
            .expect("function output item is done");
        assert_eq!(done["item"]["id"], json!("item-1"));
        assert_eq!(done["item"]["call_id"], json!("call-1"));
        assert_eq!(
            done["item"]["name"],
            json!("lookup"),
            "Codex parses ResponseItem::FunctionCall from output_item.done and requires name"
        );
        assert_eq!(
            done["item"]["arguments"],
            json!(r#"{"q":"rust"}"#),
            "Codex executes tools with the arguments string from the done item"
        );
    }

    #[test]
    fn encoders_preserve_nonempty_initial_tool_arguments() {
        let events = [
            started("stream-1", "model"),
            tool_with_arguments("call-1", Some("item-1"), 0, "lookup"),
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            completed("tool_calls", "tool_use"),
        ];

        let mut anthropic_state = StreamState::new();
        let anthropic = encode_stream_events(
            WireProtocol::AnthropicMessages,
            &events,
            &mut anthropic_state,
        )
        .expect("Anthropic arguments encode");
        let anthropic_start = anthropic
            .iter()
            .map(event_data)
            .find(|value| value["type"] == json!("content_block_start"))
            .expect("Anthropic tool block starts");
        assert_eq!(
            anthropic_start["content_block"]["input"],
            json!({ "seed": 1 })
        );

        let mut chat_state = StreamState::new();
        let chat = encode_stream_events(WireProtocol::OpenAiChat, &events, &mut chat_state)
            .expect("Chat arguments encode");
        let chat_start = chat
            .iter()
            .map(event_data)
            .find(|value| value.pointer("/choices/0/delta/tool_calls/0").is_some())
            .expect("Chat tool call starts");
        assert_eq!(
            chat_start["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            json!(r#"{"seed":1}"#)
        );

        let mut responses_state = StreamState::new();
        let responses =
            encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut responses_state)
                .expect("Responses arguments encode");
        let responses_start = responses
            .iter()
            .map(event_data)
            .find(|value| value["type"] == json!("response.output_item.added"))
            .expect("Responses tool item starts");
        assert_eq!(responses_start["item"]["arguments"], json!(r#"{"seed":1}"#));
    }

    #[test]
    fn stream_encoder_rejects_eof_without_terminal_and_hides_failure_details() {
        let events = [started("chat-1", "model")];
        let mut state = StreamState::new();
        encode_stream_events(WireProtocol::OpenAiChat, &events, &mut state).expect("start encodes");
        assert_eq!(state.finish_eof(), Err(BridgeError::InvalidUpstream));

        let mut failed_state = StreamState::new();
        let output = encode_stream_events(
            WireProtocol::OpenAiChat,
            &[
                started("chat-1", "model"),
                StreamEventIr::Failed(BridgeError::Unsupported {
                    field: "api_key=secret prompt=secret".to_string(),
                }),
            ],
            &mut failed_state,
        )
        .expect("failure encodes");
        let serialized = output
            .iter()
            .map(|frame| frame.data.as_str())
            .collect::<String>();
        assert!(!serialized.contains("secret"));
        assert!(serialized.contains("invalid_upstream"));
    }

    #[test]
    fn responses_accepts_function_call_arguments_done_after_deltas() {
        let mut state = StreamState::new();
        let frames = [
            frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"item-1","type":"function_call","call_id":"call-1","name":"lookup","arguments":""}}"#,
            ),
            frame(
                Some("response.function_call_arguments.delta"),
                r#"{"type":"response.function_call_arguments.delta","item_id":"item-1","output_index":0,"delta":"{\"city\":\"fixture-"}"#,
            ),
            frame(
                Some("response.function_call_arguments.delta"),
                r#"{"type":"response.function_call_arguments.delta","item_id":"item-1","output_index":0,"delta":"city\"}"}"#,
            ),
            frame(
                Some("response.function_call_arguments.done"),
                r#"{"type":"response.function_call_arguments.done","item_id":"item-1","output_index":0,"arguments":"{\"city\":\"fixture-city\"}"}"#,
            ),
        ];

        for frame in &frames {
            decode_stream_frame(WireProtocol::OpenAiResponses, frame, &mut state)
                .expect("valid Responses tool lifecycle is accepted");
        }
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.function_call_arguments.done"),
                    r#"{"type":"response.function_call_arguments.done","item_id":"item-1","output_index":0,"arguments":"{\"city\":\"fixture-city\"}"}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn responses_tool_events_bind_item_id_to_output_index() {
        let mut state = StreamState::new();
        for frame in [
            frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"item-0","type":"function_call","call_id":"call-0","name":"zero","arguments":""}}"#,
            ),
            frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":1,"item":{"id":"item-1","type":"function_call","call_id":"call-1","name":"one","arguments":""}}"#,
            ),
        ] {
            decode_stream_frame(WireProtocol::OpenAiResponses, &frame, &mut state)
                .expect("tool item starts");
        }

        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.function_call_arguments.delta"),
                    r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"item-1","delta":"{}"}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.function_call_arguments.done"),
                    r#"{"type":"response.function_call_arguments.done","output_index":0,"item_id":"item-1","arguments":"{}"}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.done"),
                    r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"item-0","type":"function_call","call_id":"call-1","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.done"),
                    r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"item-1","type":"function_call","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_text.delta"),
                    r#"{"type":"response.output_text.delta","output_index":0,"item_id":"msg-1","delta":"late"}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn stream_usage_rejects_wrong_types_and_unknown_fields() {
        let mut anthropic_state = StreamState::new();
        assert_eq!(
            decode_stream_frame(
                WireProtocol::AnthropicMessages,
                &frame(
                    Some("message_start"),
                    r#"{"type":"message_start","message":{"id":"msg-1","model":"model","usage":{"input_tokens":"bad"}}}"#,
                ),
                &mut anthropic_state,
            ),
            Err(BridgeError::InvalidUpstream)
        );

        let mut chat_state = StreamState::new();
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiChat,
                &frame(
                    None,
                    r#"{"id":"chat-1","model":"model","choices":[],"usage":{"prompt_tokens":"bad"}}"#,
                ),
                &mut chat_state,
            ),
            Err(BridgeError::InvalidUpstream)
        );

        let mut responses_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut responses_state,
        )
        .expect("Responses stream starts");
        assert!(matches!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.completed"),
                    r#"{"type":"response.completed","response":{"id":"resp-1","model":"model","status":"completed","usage":{"unknown":1}}}"#,
                ),
                &mut responses_state,
            ),
            Err(BridgeError::InvalidUpstream) | Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn stream_response_identity_must_remain_stable() {
        let mut chat_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiChat,
            &frame(
                None,
                r#"{"id":"chat-1","model":"model","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
            ),
            &mut chat_state,
        )
        .expect("Chat stream starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiChat,
                &frame(
                    None,
                    r#"{"id":"chat-2","model":"model","choices":[{"index":0,"delta":{"content":"late"},"finish_reason":null}]}"#,
                ),
                &mut chat_state,
            ),
            Err(BridgeError::InvalidUpstream)
        );

        let mut responses_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut responses_state,
        )
        .expect("Responses stream starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.in_progress"),
                    r#"{"type":"response.in_progress","response":{"id":"resp-2","model":"model","status":"in_progress"}}"#,
                ),
                &mut responses_state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.added"),
                    r#"{"type":"response.output_item.added","response_id":"resp-2","output_index":0,"item":{"id":"item-0","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
                ),
                &mut responses_state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","response_id":"resp-1","output_index":0,"item":{"id":"msg-1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
            ),
            &mut responses_state,
        )
        .expect("message item starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_text.delta"),
                    r#"{"type":"response.output_text.delta","response_id":"resp-2","output_index":0,"delta":"late"}"#,
                ),
                &mut responses_state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
    }

    #[test]
    fn wire_indexes_are_not_required_to_be_contiguous_tool_ordinals() {
        let mut state = StreamState::new();
        let frames = [
            frame(
                Some("message_start"),
                r#"{"type":"message_start","message":{"id":"msg-1","model":"model","usage":{"input_tokens":1}}}"#,
            ),
            frame(
                Some("content_block_start"),
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            frame(
                Some("content_block_start"),
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call-1","name":"lookup","input":{}}}"#,
            ),
            frame(
                Some("content_block_delta"),
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            ),
            frame(
                Some("content_block_stop"),
                r#"{"type":"content_block_stop","index":2}"#,
            ),
        ];

        for frame in &frames {
            decode_stream_frame(WireProtocol::AnthropicMessages, frame, &mut state)
                .expect("wire block indexes may include text blocks");
        }
    }

    #[test]
    fn anthropic_initial_tool_input_is_kept_when_no_argument_delta_follows() {
        let mut state = StreamState::new();
        let events = [
            started("msg-1", "model"),
            StreamEventIr::ToolCallStarted(ToolCallIr {
                call_id: "call-1".to_string(),
                item_id: None,
                index: 0,
                name: "lookup".to_string(),
                arguments: json!({ "seed": 1 }),
            }),
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
        ];

        for event in &events {
            state
                .apply_event(event)
                .expect("initial tool input is complete JSON");
        }
        assert!(!state.is_terminal());
    }

    #[test]
    fn unknown_stream_blocks_fail_closed() {
        let mut anthropic_state = StreamState::new();
        anthropic_state
            .apply_event(&started("msg-1", "model"))
            .expect("stream starts");
        assert!(matches!(
            super::super::anthropic::decode_stream_frame(
                &frame(
                    Some("content_block_start"),
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"audio"}}"#
                ),
                &mut anthropic_state,
            ),
            Err(BridgeError::Unsupported { .. })
        ));

        let mut responses_state = StreamState::new();
        responses_state
            .apply_event(&started("resp-1", "model"))
            .expect("stream starts");
        assert!(matches!(
            super::super::responses::decode_stream_frame(
                &frame(
                    Some("response.output_item.added"),
                    r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"audio"}}"#
                ),
                &mut responses_state,
            ),
            Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn failed_first_decode_does_not_bind_protocol() {
        let mut state = StreamState::new();
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiChat,
                &frame(None, "{not-json"),
                &mut state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut state,
        )
        .expect("failed decode did not bind the protocol");
    }

    #[test]
    fn responses_mixed_output_items_receive_unique_output_indexes() {
        let events = [
            started("resp-1", "model"),
            StreamEventIr::TextDelta {
                text: "answer".to_string(),
            },
            tool("call-1", Some("item-1"), 0, "lookup"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: "{}".to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            completed("tool_calls", "tool_use"),
        ];
        let mut state = StreamState::new();
        let frames = encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut state)
            .expect("mixed Responses stream encodes");
        let indexes: Vec<_> = frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some("response.output_item.added"))
            .map(|frame| {
                event_data(frame)["output_index"]
                    .as_u64()
                    .expect("output index")
            })
            .collect();
        assert_eq!(indexes, vec![0, 1]);
    }

    #[test]
    fn responses_text_done_contains_accumulated_text() {
        let events = [
            started("resp-1", "model"),
            StreamEventIr::TextDelta {
                text: "hello".to_string(),
            },
            StreamEventIr::TextDelta {
                text: " world".to_string(),
            },
            completed("stop", "end_turn"),
        ];
        let mut state = StreamState::new();
        let frames = encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut state)
            .expect("Responses text stream encodes");
        let done = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.output_text.done"))
            .map(event_data)
            .expect("Responses text done event exists");
        assert_eq!(done["text"], json!("hello world"));
    }

    #[test]
    fn responses_tool_fixture_decodes_until_terminal_and_rejects_malformed_tail() {
        let fixture =
            include_str!("../../../tests/fixtures/runtime_bridge/responses_tool_stream.sse");
        let (valid, _) = fixture
            .split_once("event: response.malformed")
            .expect("fixture has a malformed tail");
        let mut decoder = SseDecoder::default();
        let mut frames = decoder.feed(valid.as_bytes()).expect("fixture SSE decodes");
        frames.extend(decoder.finish().expect("fixture has no partial frame"));
        let mut state = StreamState::new();
        for frame in &frames {
            decode_stream_frame(WireProtocol::OpenAiResponses, frame, &mut state).unwrap_or_else(
                |error| panic!("valid fixture frame {:?} failed: {error:?}", frame.event),
            );
        }
        assert!(state.is_terminal());
        assert_eq!(state.usage().and_then(|usage| usage.total_tokens), Some(20));

        let mut malformed_decoder = SseDecoder::default();
        let malformed_frames = malformed_decoder
            .feed(b"event: response.malformed\ndata: {\"type\":\"response.malformed\"}\n\n")
            .expect("malformed event has valid SSE framing");
        let malformed_frames = malformed_frames.into_iter().chain(
            malformed_decoder
                .finish()
                .expect("malformed frame is complete"),
        );
        let malformed_frame = malformed_frames
            .into_iter()
            .next()
            .expect("malformed frame exists");
        let mut malformed_state = StreamState::new();
        assert!(matches!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &malformed_frame,
                &mut malformed_state,
            ),
            Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn chat_tool_fixture_finishes_open_tools_at_done() {
        let fixture = include_str!("../../../tests/fixtures/runtime_bridge/chat_tool_stream.sse");
        let (valid, _) = fixture
            .split_once("event: malformed")
            .expect("fixture has a malformed tail");
        let mut decoder = SseDecoder::default();
        let mut frames = decoder.feed(valid.as_bytes()).expect("fixture SSE decodes");
        frames.extend(decoder.finish().expect("fixture has no partial frame"));
        let mut state = StreamState::new();
        for frame in &frames {
            decode_stream_frame(WireProtocol::OpenAiChat, frame, &mut state)
                .expect("valid Chat tool frame decodes");
        }
        assert!(state.is_terminal());
        assert_eq!(state.usage().and_then(|usage| usage.total_tokens), Some(20));
    }

    #[test]
    fn zen_tool_stream_with_tail_tolerates_post_done_frame() {
        let fixture =
            include_str!("../../../tests/fixtures/runtime_bridge/zen_tool_stream_with_tail.sse");
        let mut decoder = SseDecoder::default();
        let mut frames = decoder
            .feed(fixture.as_bytes())
            .expect("fixture SSE decodes");
        frames.extend(decoder.finish().expect("fixture has no partial frame"));
        let mut state = StreamState::new();
        let mut completed = 0;
        for frame in &frames {
            for event in decode_stream_frame(WireProtocol::OpenAiChat, frame, &mut state)
                .expect("every Zen frame decodes, including the post-DONE tail frame")
            {
                if matches!(event, StreamEventIr::Completed(_)) {
                    completed += 1;
                }
            }
        }
        assert_eq!(completed, 1);
        assert!(state.is_terminal());
        let call = state
            .tool_state("call_73273ba8cb504a4fbbb28f1d")
            .expect("tool call is tracked");
        assert_eq!(call.name(), "lookup");
        assert!(call.is_completed());
        let arguments: Value = serde_json::from_str(call.arguments()).expect("arguments are JSON");
        assert_eq!(
            arguments.get("q").and_then(Value::as_str),
            Some("zen fixture")
        );
        assert_eq!(
            state.usage().and_then(|usage| usage.total_tokens),
            Some(446)
        );
    }

    #[test]
    fn responses_output_item_indexes_are_global_and_contiguous() {
        let mut state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut state,
        )
        .expect("response starts");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"msg-1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
            ),
            &mut state,
        )
        .expect("message item starts");
        assert!(matches!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.added"),
                    r#"{"type":"response.output_item.added","output_index":2,"item":{"id":"reason-1","type":"reasoning","status":"in_progress","summary":[]}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        ));
    }

    #[test]
    fn anthropic_non_tool_blocks_require_one_stop_before_message_stop() {
        let mut state = StreamState::new();
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("message_start"),
                r#"{"type":"message_start","message":{"id":"msg-1","model":"model","usage":{"input_tokens":1}}}"#,
            ),
            &mut state,
        )
        .expect("message starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("content_block_start"),
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            &mut state,
        )
        .expect("text block starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("message_delta"),
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            ),
            &mut state,
        )
        .expect("completion metadata is accepted");

        assert_eq!(
            decode_stream_frame(
                WireProtocol::AnthropicMessages,
                &frame(Some("message_stop"), r#"{"type":"message_stop"}"#),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("content_block_stop"),
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            &mut state,
        )
        .expect("text block stops");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::AnthropicMessages,
                &frame(
                    Some("content_block_stop"),
                    r#"{"type":"content_block_stop","index":0}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(Some("message_stop"), r#"{"type":"message_stop"}"#),
            &mut state,
        )
        .expect("closed text block permits message stop");
    }

    #[test]
    fn responses_non_tool_items_bind_identity_and_close_once() {
        let mut state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut state,
        )
        .expect("response starts");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"msg-1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
            ),
            &mut state,
        )
        .expect("message item starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_text.delta"),
                    r#"{"type":"response.output_text.delta","output_index":0,"item_id":"msg-2","delta":"wrong"}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_text.delta"),
                r#"{"type":"response.output_text.delta","output_index":0,"item_id":"msg-1","delta":"answer"}"#,
            ),
            &mut state,
        )
        .expect("matching text delta is accepted");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.done"),
                    r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"msg-2","type":"message","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.done"),
                r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"msg-1","type":"message","status":"completed"}}"#,
            ),
            &mut state,
        )
        .expect("matching item closes");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.done"),
                    r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"msg-1","type":"message","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn tool_arguments_count_toward_cumulative_output_budget() {
        let mut initial = StreamState::with_limits(1024, 9);
        initial
            .apply_event(&started("resp-1", "model"))
            .expect("stream starts");
        assert_eq!(
            initial.apply_event(&tool_with_arguments("call-1", None, 0, "lookup")),
            Err(BridgeError::ResourceLimit)
        );

        let mut final_arguments = StreamState::with_limits(1024, 9);
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-2","model":"model","status":"in_progress"}}"#,
            ),
            &mut final_arguments,
        )
        .expect("response starts");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"item-1","type":"function_call","status":"in_progress","call_id":"call-1","name":"lookup","arguments":""}}"#,
            ),
            &mut final_arguments,
        )
        .expect("tool starts");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.function_call_arguments.delta"),
                r#"{"output_index":0,"item_id":"item-1","delta":"{\"seed\":"}"#,
            ),
            &mut final_arguments,
        )
        .expect("argument prefix is accepted");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.function_call_arguments.done"),
                    r#"{"output_index":0,"item_id":"item-1","arguments":"{\"seed\":1}"}"#,
                ),
                &mut final_arguments,
            ),
            Err(BridgeError::ResourceLimit)
        );
    }

    #[test]
    fn responses_terminal_event_must_match_embedded_status() {
        let mut state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut state,
        )
        .expect("response starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.incomplete"),
                    r#"{"type":"response.incomplete","response":{"id":"resp-1","model":"model","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.failed"),
                    r#"{"type":"response.failed","response":{"id":"resp-1","model":"model","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
    }

    #[test]
    fn responses_terminal_requires_all_output_items_to_close() {
        let mut state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut state,
        )
        .expect("response starts");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"msg-1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
            ),
            &mut state,
        )
        .expect("message item starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.completed"),
                    r#"{"type":"response.completed","response":{"id":"resp-1","model":"model","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.done"),
                r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"msg-1","type":"message","status":"completed"}}"#,
            ),
            &mut state,
        )
        .expect("message item closes");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.completed"),
                r#"{"type":"response.completed","response":{"id":"resp-1","model":"model","status":"completed"}}"#,
            ),
            &mut state,
        )
        .expect("closed item permits completion");
    }

    #[test]
    fn anthropic_rejects_delta_after_content_block_stop() {
        let mut state = StreamState::new();
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("message_start"),
                r#"{"type":"message_start","message":{"id":"msg-1","model":"model","usage":{"input_tokens":1}}}"#,
            ),
            &mut state,
        )
        .expect("message starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("content_block_start"),
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            &mut state,
        )
        .expect("text block starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("content_block_stop"),
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            &mut state,
        )
        .expect("text block stops");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::AnthropicMessages,
                &frame(
                    Some("content_block_delta"),
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"late"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn stream_item_metadata_has_a_bounded_cardinality() {
        let mut state = StreamState::new();
        state
            .apply_event(&started("resp-1", "model"))
            .expect("stream starts");
        for index in 0..DEFAULT_MAX_STREAM_ITEMS {
            state
                .apply_event(&tool(&format!("call-{index}"), None, index, "lookup"))
                .expect("bounded tool item starts");
        }
        assert_eq!(
            state.apply_event(&tool(
                "call-over-limit",
                None,
                DEFAULT_MAX_STREAM_ITEMS,
                "lookup",
            )),
            Err(BridgeError::ResourceLimit)
        );
    }

    #[test]
    fn encoder_namespaces_internal_ids_from_user_tool_ids() {
        let events = [
            started("stream-1", "model"),
            StreamEventIr::TextDelta {
                text: "answer".to_string(),
            },
            tool("text", Some("__message__"), 0, "lookup"),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "text".to_string(),
                delta: "{}".to_string(),
            },
            StreamEventIr::ToolCallFinished {
                call_id: "text".to_string(),
            },
            completed("tool_calls", "tool_use"),
        ];

        let mut anthropic_state = StreamState::new();
        let anthropic = encode_stream_events(
            WireProtocol::AnthropicMessages,
            &events,
            &mut anthropic_state,
        )
        .expect("Anthropic encoder keeps block namespaces separate");
        let anthropic_indexes: Vec<_> = anthropic
            .iter()
            .filter(|frame| frame.event.as_deref() == Some("content_block_start"))
            .map(|frame| event_data(frame)["index"].as_u64().expect("block index"))
            .collect();
        assert_eq!(anthropic_indexes.len(), 2);
        assert_ne!(anthropic_indexes[0], anthropic_indexes[1]);

        let mut responses_state = StreamState::new();
        let responses =
            encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut responses_state)
                .expect("Responses encoder keeps item namespaces separate");
        let item_ids: Vec<_> = responses
            .iter()
            .filter(|frame| frame.event.as_deref() == Some("response.output_item.added"))
            .map(|frame| event_data(frame)["item"]["id"].clone())
            .collect();
        assert_eq!(item_ids.len(), 2);
        assert_ne!(item_ids[0], item_ids[1]);
    }

    #[test]
    fn responses_non_tool_item_ids_are_global_and_message_role_is_assistant() {
        let mut duplicate = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
            ),
            &mut duplicate,
        )
        .expect("response starts");
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.output_item.added"),
                r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"item-1","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
            ),
            &mut duplicate,
        )
        .expect("message starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.added"),
                    r#"{"type":"response.output_item.added","output_index":1,"item":{"id":"item-1","type":"reasoning","status":"in_progress","summary":[]}}"#,
                ),
                &mut duplicate,
            ),
            Err(BridgeError::ToolState)
        );

        let mut wrong_role = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiResponses,
            &frame(
                Some("response.created"),
                r#"{"type":"response.created","response":{"id":"resp-2","model":"model","status":"in_progress"}}"#,
            ),
            &mut wrong_role,
        )
        .expect("response starts");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.output_item.added"),
                    r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"item-2","type":"message","role":"user","status":"in_progress","content":[]}}"#,
                ),
                &mut wrong_role,
            ),
            Err(BridgeError::InvalidUpstream)
        );
    }

    #[test]
    fn responses_incomplete_encoder_preserves_terminal_status() {
        let events = [
            started("resp-1", "model"),
            StreamEventIr::TextDelta {
                text: "partial".to_string(),
            },
            StreamEventIr::Completed(CompletionIr {
                finish_reason: Some("length".to_string()),
                stop_reason: Some("max_tokens".to_string()),
                status: Some("incomplete".to_string()),
                error: None,
            }),
        ];
        let mut state = StreamState::new();
        let frames = encode_stream_events(WireProtocol::OpenAiResponses, &events, &mut state)
            .expect("incomplete Responses stream encodes");
        let terminal = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.incomplete"))
            .expect("incomplete terminal event");
        let terminal = event_data(terminal);
        assert_eq!(terminal["response"]["status"], json!("incomplete"));
        let item_done = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.output_item.done"))
            .expect("message item closes");
        assert_eq!(event_data(item_done)["item"]["status"], json!("incomplete"));
    }

    #[test]
    fn responses_start_events_require_in_progress_status() {
        let mut state = StreamState::new();
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.created"),
                    r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"completed"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.in_progress"),
                    r#"{"type":"response.in_progress","response":{"id":"resp-1","model":"model"}}"#,
                ),
                &mut state,
            ),
            Err(BridgeError::InvalidUpstream)
        );
    }

    #[test]
    fn chat_and_anthropic_reject_payload_after_finish_metadata() {
        let mut chat_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiChat,
            &frame(
                None,
                r#"{"id":"chat-1","model":"model","choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
            ),
            &mut chat_state,
        )
        .expect("Chat finish metadata is accepted");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::OpenAiChat,
                &frame(
                    None,
                    r#"{"id":"chat-1","model":"model","choices":[{"index":0,"delta":{"content":"late"},"finish_reason":null}]}"#,
                ),
                &mut chat_state,
            ),
            Err(BridgeError::ToolState)
        );

        let mut anthropic_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("message_start"),
                r#"{"type":"message_start","message":{"id":"msg-1","model":"model","usage":{"input_tokens":1}}}"#,
            ),
            &mut anthropic_state,
        )
        .expect("Anthropic stream starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("content_block_start"),
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            &mut anthropic_state,
        )
        .expect("Anthropic text block starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("message_delta"),
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            ),
            &mut anthropic_state,
        )
        .expect("Anthropic finish metadata is accepted");
        assert_eq!(
            decode_stream_frame(
                WireProtocol::AnthropicMessages,
                &frame(
                    Some("content_block_delta"),
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"late"}}"#,
                ),
                &mut anthropic_state,
            ),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn stream_decoders_reject_unknown_event_fields_and_chat_refusal_delta() {
        let mut chat_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::OpenAiChat,
            &frame(
                None,
                r#"{"id":"chat-1","model":"model","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
            ),
            &mut chat_state,
        )
        .expect("Chat stream starts");
        assert!(matches!(
            decode_stream_frame(
                WireProtocol::OpenAiChat,
                &frame(
                    None,
                    r#"{"id":"chat-1","model":"model","choices":[{"index":0,"delta":{"refusal":"no"},"finish_reason":null}]}"#,
                ),
                &mut chat_state,
            ),
            Err(BridgeError::Unsupported { .. })
        ));

        let mut anthropic_state = StreamState::new();
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("message_start"),
                r#"{"type":"message_start","message":{"id":"msg-1","model":"model","usage":{"input_tokens":1}}}"#,
            ),
            &mut anthropic_state,
        )
        .expect("Anthropic stream starts");
        decode_stream_frame(
            WireProtocol::AnthropicMessages,
            &frame(
                Some("content_block_start"),
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            &mut anthropic_state,
        )
        .expect("Anthropic text block starts");
        assert!(matches!(
            decode_stream_frame(
                WireProtocol::AnthropicMessages,
                &frame(
                    Some("content_block_delta"),
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok","unexpected":true}}"#,
                ),
                &mut anthropic_state,
            ),
            Err(BridgeError::Unsupported { .. })
        ));

        let mut responses_state = StreamState::new();
        assert!(matches!(
            decode_stream_frame(
                WireProtocol::OpenAiResponses,
                &frame(
                    Some("response.created"),
                    r#"{"type":"response.created","unexpected":true,"response":{"id":"resp-1","model":"model","status":"in_progress"}}"#,
                ),
                &mut responses_state,
            ),
            Err(BridgeError::Unsupported { .. })
        ));
    }
}
