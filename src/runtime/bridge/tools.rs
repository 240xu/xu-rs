use std::collections::BTreeMap;
use std::fmt;

use super::ir::BridgeError;

pub const DEFAULT_MAX_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Eq, PartialEq)]
pub struct ToolCallState {
    call_id: String,
    item_id: Option<String>,
    index: usize,
    name: String,
    arguments: String,
    arguments_finalized: bool,
    completed: bool,
    max_argument_bytes: usize,
}

impl fmt::Debug for ToolCallState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCallState")
            .field("call_id", &self.call_id)
            .field("item_id", &self.item_id)
            .field("index", &self.index)
            .field("name", &self.name)
            .field("argument_bytes", &self.arguments.len())
            .field("completed", &self.completed)
            .finish()
    }
}

impl ToolCallState {
    pub fn new(
        call_id: impl Into<String>,
        item_id: Option<String>,
        index: usize,
        name: impl Into<String>,
        max_argument_bytes: usize,
    ) -> Result<Self, BridgeError> {
        let call_id = call_id.into();
        let name = name.into();
        if call_id.trim().is_empty()
            || name.trim().is_empty()
            || item_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            return Err(BridgeError::ToolState);
        }
        Ok(Self {
            call_id,
            item_id,
            index,
            name,
            arguments: String::new(),
            arguments_finalized: false,
            completed: false,
            max_argument_bytes,
        })
    }

    pub fn with_default_limit(
        call_id: impl Into<String>,
        item_id: Option<String>,
        index: usize,
        name: impl Into<String>,
    ) -> Result<Self, BridgeError> {
        Self::new(
            call_id,
            item_id,
            index,
            name,
            DEFAULT_MAX_TOOL_ARGUMENT_BYTES,
        )
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn item_id(&self) -> Option<&str> {
        self.item_id.as_deref()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> &str {
        &self.arguments
    }

    pub fn set_arguments_if_empty(&mut self, arguments: &str) -> Result<(), BridgeError> {
        if self.completed || self.arguments_finalized {
            return Err(BridgeError::ToolState);
        }
        if self.arguments.is_empty() {
            return self.push_arguments(arguments);
        }
        if self.arguments == arguments {
            Ok(())
        } else {
            Err(BridgeError::ToolState)
        }
    }

    pub fn set_arguments_final(&mut self, arguments: &str) -> Result<(), BridgeError> {
        if self.completed || self.arguments_finalized {
            return Err(BridgeError::ToolState);
        }
        if arguments.len() > self.max_argument_bytes {
            return Err(BridgeError::ResourceLimit);
        }
        let parsed: serde_json::Value =
            serde_json::from_str(arguments).map_err(|_| BridgeError::ToolState)?;
        if !parsed.is_object() {
            return Err(BridgeError::ToolState);
        }
        self.arguments = arguments.to_string();
        self.arguments_finalized = true;
        Ok(())
    }

    pub fn is_completed(&self) -> bool {
        self.completed
    }

    pub fn push_arguments(&mut self, fragment: &str) -> Result<(), BridgeError> {
        if self.completed || self.arguments_finalized {
            return Err(BridgeError::ToolState);
        }
        if self.arguments.len().saturating_add(fragment.len()) > self.max_argument_bytes {
            return Err(BridgeError::ResourceLimit);
        }
        self.arguments.push_str(fragment);
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), BridgeError> {
        if self.completed {
            return Err(BridgeError::ToolState);
        }
        self.completed = true;
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ToolCallTracker {
    calls: BTreeMap<usize, ToolCallState>,
    call_ids: BTreeMap<String, usize>,
    item_ids: BTreeMap<String, usize>,
    next_index: usize,
    max_argument_bytes: usize,
}

impl fmt::Debug for ToolCallTracker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCallTracker")
            .field("call_count", &self.calls.len())
            .field("next_index", &self.next_index)
            .finish()
    }
}

impl ToolCallTracker {
    pub fn new(max_argument_bytes: usize) -> Self {
        Self {
            calls: BTreeMap::new(),
            call_ids: BTreeMap::new(),
            item_ids: BTreeMap::new(),
            next_index: 0,
            max_argument_bytes,
        }
    }

    pub fn with_default_limit() -> Self {
        Self::new(DEFAULT_MAX_TOOL_ARGUMENT_BYTES)
    }

    pub fn start(
        &mut self,
        call_id: impl Into<String>,
        item_id: Option<&str>,
        index: usize,
        name: impl Into<String>,
    ) -> Result<(), BridgeError> {
        let call_id = call_id.into();
        let item_id = item_id.map(str::to_owned);
        if index != self.next_index
            || self.call_ids.contains_key(&call_id)
            || item_id
                .as_ref()
                .is_some_and(|value| self.item_ids.contains_key(value))
        {
            return Err(BridgeError::ToolState);
        }

        let state = ToolCallState::new(
            call_id.clone(),
            item_id.clone(),
            index,
            name,
            self.max_argument_bytes,
        )?;
        let next_index = self
            .next_index
            .checked_add(1)
            .ok_or(BridgeError::ResourceLimit)?;

        self.call_ids.insert(call_id, index);
        if let Some(item_id) = item_id {
            self.item_ids.insert(item_id, index);
        }
        self.calls.insert(index, state);
        self.next_index = next_index;
        Ok(())
    }

    pub fn get(&self, call_id: &str, item_id: Option<&str>) -> Result<&ToolCallState, BridgeError> {
        let index = self.identity_index(call_id, item_id)?;
        self.calls.get(&index).ok_or(BridgeError::ToolState)
    }

    pub fn states(&self) -> impl Iterator<Item = &ToolCallState> {
        self.calls.values()
    }

    pub fn push_arguments(
        &mut self,
        call_id: &str,
        item_id: Option<&str>,
        fragment: &str,
    ) -> Result<(), BridgeError> {
        let index = self.identity_index(call_id, item_id)?;
        self.calls
            .get_mut(&index)
            .ok_or(BridgeError::ToolState)?
            .push_arguments(fragment)
    }

    pub fn set_arguments_if_empty(
        &mut self,
        call_id: &str,
        item_id: Option<&str>,
        arguments: &str,
    ) -> Result<(), BridgeError> {
        let index = self.identity_index(call_id, item_id)?;
        self.calls
            .get_mut(&index)
            .ok_or(BridgeError::ToolState)?
            .set_arguments_if_empty(arguments)
    }

    pub fn set_arguments_final(
        &mut self,
        call_id: &str,
        item_id: Option<&str>,
        arguments: &str,
    ) -> Result<(), BridgeError> {
        let index = self.identity_index(call_id, item_id)?;
        self.calls
            .get_mut(&index)
            .ok_or(BridgeError::ToolState)?
            .set_arguments_final(arguments)
    }

    pub fn finish(&mut self, call_id: &str, item_id: Option<&str>) -> Result<(), BridgeError> {
        let index = self.identity_index(call_id, item_id)?;
        let state = self.calls.get(&index).ok_or(BridgeError::ToolState)?;
        let parsed: serde_json::Value =
            serde_json::from_str(state.arguments()).map_err(|_| BridgeError::ToolState)?;
        if !parsed.is_object() {
            return Err(BridgeError::ToolState);
        }
        self.calls
            .get_mut(&index)
            .ok_or(BridgeError::ToolState)?
            .finish()
    }

    fn identity_index(&self, call_id: &str, item_id: Option<&str>) -> Result<usize, BridgeError> {
        if call_id.trim().is_empty() || item_id.is_some_and(|value| value.trim().is_empty()) {
            return Err(BridgeError::ToolState);
        }
        let index = self
            .call_ids
            .get(call_id)
            .copied()
            .ok_or(BridgeError::ToolState)?;
        let state = self.calls.get(&index).ok_or(BridgeError::ToolState)?;
        if state.item_id() != item_id {
            return Err(BridgeError::ToolState);
        }
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_rejects_non_object_fragmented_arguments_at_finish() {
        let mut tracker = ToolCallTracker::new(64);
        tracker
            .start("call-1", Some("item-1"), 0, "lookup")
            .expect("call starts");
        tracker
            .push_arguments("call-1", Some("item-1"), "[1")
            .expect("first fragment appends");
        tracker
            .push_arguments("call-1", Some("item-1"), "]")
            .expect("second fragment appends");
        assert_eq!(
            tracker.finish("call-1", Some("item-1")),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn tracker_rejects_missing_call_identity() {
        let mut tracker = ToolCallTracker::new(64);
        assert_eq!(
            tracker.start("", Some("item-1"), 0, "lookup"),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            tracker.start("call-1", Some(""), 0, "lookup"),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn tracker_preserves_parallel_call_identity_through_fragmented_arguments() {
        let mut tracker = ToolCallTracker::new(64);
        tracker
            .start("call-1", Some("item-1"), 0, "one")
            .expect("first call starts");
        tracker
            .start("call-2", Some("item-2"), 1, "two")
            .expect("second call starts");
        tracker
            .push_arguments("call-2", Some("item-2"), "{\"v\"")
            .expect("first second-call fragment appends");
        tracker
            .push_arguments("call-2", Some("item-2"), ":2}")
            .expect("second second-call fragment appends");
        assert_eq!(
            tracker
                .get("call-2", Some("item-2"))
                .expect("call exists")
                .arguments(),
            "{\"v\":2}"
        );
        tracker
            .finish("call-2", Some("item-2"))
            .expect("object arguments finish");
        assert!(tracker.get("call-1", Some("item-1")).is_ok());
    }
}
