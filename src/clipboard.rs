use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::proto;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HybridTimestamp {
    pub physical_millis: u64,
    pub logical: u64,
    pub client_id: String,
}

impl Ord for HybridTimestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.physical_millis, self.logical, self.client_id.as_str()).cmp(&(
            other.physical_millis,
            other.logical,
            other.client_id.as_str(),
        ))
    }
}

impl PartialOrd for HybridTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<&HybridTimestamp> for proto::ClipboardTimestamp {
    fn from(timestamp: &HybridTimestamp) -> Self {
        Self {
            physical_millis: timestamp.physical_millis,
            logical: timestamp.logical,
            client_id: timestamp.client_id.clone(),
        }
    }
}

impl From<proto::ClipboardTimestamp> for HybridTimestamp {
    fn from(timestamp: proto::ClipboardTimestamp) -> Self {
        Self {
            physical_millis: timestamp.physical_millis,
            logical: timestamp.logical,
            client_id: timestamp.client_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOperation {
    Copy,
    Cut,
}

impl ClipboardOperation {
    fn to_proto(self) -> proto::ClipboardOperation {
        match self {
            Self::Copy => proto::ClipboardOperation::Copy,
            Self::Cut => proto::ClipboardOperation::Cut,
        }
    }

    fn from_proto(operation: i32) -> Option<Self> {
        match proto::ClipboardOperation::try_from(operation).ok()? {
            proto::ClipboardOperation::Copy => Some(Self::Copy),
            proto::ClipboardOperation::Cut => Some(Self::Cut),
            proto::ClipboardOperation::Unspecified => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardItem {
    pub path: PathBuf,
    pub operation: ClipboardOperation,
    pub timestamp: HybridTimestamp,
}

impl ClipboardItem {
    pub fn to_proto(&self) -> proto::ClipboardItem {
        proto::ClipboardItem {
            path: self.path.to_string_lossy().into_owned(),
            operation: self.operation.to_proto().into(),
            timestamp: Some((&self.timestamp).into()),
        }
    }

    pub fn from_proto(item: proto::ClipboardItem) -> Option<Self> {
        Some(Self {
            path: PathBuf::from(item.path),
            operation: ClipboardOperation::from_proto(item.operation)?,
            timestamp: item.timestamp?.into(),
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClipboardState {
    items: HashMap<PathBuf, ClipboardItem>,
    clear_timestamp: Option<HybridTimestamp>,
}

impl ClipboardState {
    pub fn items(&self) -> impl Iterator<Item = &ClipboardItem> {
        self.items.values()
    }

    pub fn clear_timestamp(&self) -> Option<&HybridTimestamp> {
        self.clear_timestamp.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_changes(&self) -> bool {
        !self.items.is_empty() || self.clear_timestamp.is_some()
    }

    pub fn copied_paths(&self) -> HashSet<PathBuf> {
        self.paths_for(ClipboardOperation::Copy)
    }

    pub fn cut_paths(&self) -> HashSet<PathBuf> {
        self.paths_for(ClipboardOperation::Cut)
    }

    pub fn remove_client_items(&mut self, client_id: &str) -> usize {
        let original_len = self.items.len();
        self.items
            .retain(|_, item| item.timestamp.client_id != client_id);
        original_len - self.items.len()
    }

    pub fn apply_item(&mut self, item: ClipboardItem) {
        if self
            .clear_timestamp
            .as_ref()
            .is_some_and(|cleared_at| item.timestamp <= *cleared_at)
        {
            return;
        }

        let should_replace = self
            .items
            .get(&item.path)
            .map(|current| item.timestamp > current.timestamp)
            .unwrap_or(true);

        if should_replace {
            self.items.insert(item.path.clone(), item);
        }
    }

    pub fn apply_clear(&mut self, timestamp: HybridTimestamp) {
        let should_replace = self
            .clear_timestamp
            .as_ref()
            .map(|current| timestamp > *current)
            .unwrap_or(true);

        if !should_replace {
            return;
        }

        self.items.retain(|_, item| item.timestamp > timestamp);
        self.clear_timestamp = Some(timestamp);
    }

    pub fn reconcile(
        &mut self,
        items: impl IntoIterator<Item = ClipboardItem>,
        clear_timestamp: Option<HybridTimestamp>,
    ) {
        if let Some(timestamp) = clear_timestamp {
            self.apply_clear(timestamp);
        }
        for item in items {
            self.apply_item(item);
        }
    }

    pub fn replace_with(
        &mut self,
        items: impl IntoIterator<Item = ClipboardItem>,
        clear_timestamp: Option<HybridTimestamp>,
    ) {
        *self = Self::default();
        self.reconcile(items, clear_timestamp);
    }

    pub fn add_paths(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        operation: ClipboardOperation,
        timestamp: HybridTimestamp,
    ) -> Vec<ClipboardItem> {
        let items: Vec<_> = paths
            .into_iter()
            .map(|path| ClipboardItem {
                path,
                operation,
                timestamp: timestamp.clone(),
            })
            .collect();

        for item in items.iter().cloned() {
            self.apply_item(item);
        }
        items
    }

    pub fn observe_with(&self, clock: &mut HybridClock) {
        if let Some(timestamp) = &self.clear_timestamp {
            clock.observe(timestamp);
        }
        for item in self.items.values() {
            clock.observe(&item.timestamp);
        }
    }

    fn paths_for(&self, operation: ClipboardOperation) -> HashSet<PathBuf> {
        self.items
            .values()
            .filter(|item| item.operation == operation)
            .map(|item| item.path.clone())
            .collect()
    }
}

#[derive(Debug)]
pub struct HybridClock {
    client_id: String,
    last_physical_millis: u64,
    last_logical: u64,
}

impl HybridClock {
    pub fn new(role: &str) -> Self {
        let now = physical_millis();
        let client_id = format!(
            "{}-{}-{}",
            role,
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        Self {
            client_id,
            last_physical_millis: now,
            last_logical: 0,
        }
    }

    pub fn tick(&mut self) -> HybridTimestamp {
        let now = physical_millis();
        if now > self.last_physical_millis {
            self.last_physical_millis = now;
            self.last_logical = 0;
        } else {
            self.last_logical = self.last_logical.saturating_add(1);
        }

        HybridTimestamp {
            physical_millis: self.last_physical_millis,
            logical: self.last_logical,
            client_id: self.client_id.clone(),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn observe(&mut self, timestamp: &HybridTimestamp) {
        match timestamp.physical_millis.cmp(&self.last_physical_millis) {
            Ordering::Greater => {
                self.last_physical_millis = timestamp.physical_millis;
                self.last_logical = timestamp.logical;
            }
            Ordering::Equal => {
                self.last_logical = self.last_logical.max(timestamp.logical);
            }
            Ordering::Less => {}
        }
    }
}

pub fn get_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|runtime_dir| runtime_dir.join("fm.sock"))
        .unwrap_or_else(|| PathBuf::from("/tmp/fm.sock"))
}

fn physical_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn timestamp(millis: u64, client: &str) -> HybridTimestamp {
        HybridTimestamp {
            physical_millis: millis,
            logical: 0,
            client_id: client.into(),
        }
    }

    fn item(path: &str, operation: ClipboardOperation, millis: u64) -> ClipboardItem {
        ClipboardItem {
            path: PathBuf::from(path),
            operation,
            timestamp: timestamp(millis, "client"),
        }
    }

    #[test]
    fn newest_operation_wins_for_the_same_path() {
        let mut state = ClipboardState::default();
        state.apply_item(item("/one", ClipboardOperation::Cut, 1));
        state.apply_item(item("/one", ClipboardOperation::Copy, 2));

        assert!(state.copied_paths().contains(Path::new("/one")));
        assert!(state.cut_paths().is_empty());
    }

    #[test]
    fn different_operations_coexist_for_different_paths() {
        let mut state = ClipboardState::default();
        state.apply_item(item("/one", ClipboardOperation::Copy, 1));
        state.apply_item(item("/two", ClipboardOperation::Cut, 2));

        assert!(state.copied_paths().contains(Path::new("/one")));
        assert!(state.cut_paths().contains(Path::new("/two")));
    }

    #[test]
    fn clear_removes_only_older_operations() {
        let mut state = ClipboardState::default();
        state.apply_item(item("/old", ClipboardOperation::Copy, 1));
        state.apply_item(item("/new", ClipboardOperation::Cut, 3));
        state.apply_clear(timestamp(2, "client"));

        assert!(!state.copied_paths().contains(Path::new("/old")));
        assert!(state.cut_paths().contains(Path::new("/new")));
    }

    #[test]
    fn stale_reconciliation_cannot_resurrect_cleared_paths() {
        let mut state = ClipboardState::default();
        state.apply_clear(timestamp(2, "client"));
        state.apply_item(item("/old", ClipboardOperation::Copy, 1));

        assert!(state.is_empty());
    }
}
