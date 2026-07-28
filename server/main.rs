use std::error::Error;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, vec};

use fm::clipboard::{
    get_socket_path, ClipboardOperation, ClipboardState, HybridClock, HybridTimestamp,
};
use fm::proto;
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

struct ServerClipboard {
    state: ClipboardState,
    clock: HybridClock,
}

impl ServerClipboard {
    fn new() -> Self {
        Self {
            state: ClipboardState::default(),
            clock: HybridClock::new("server"),
        }
    }
}

/// Cleans up the socket file using RAII (only works on normal exit).
struct SocketCleanup {
    path: PathBuf,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        println!("Deleting socket at {:?}", self.path);
        if fs::remove_file(&self.path).is_err() {
            eprintln!("Could not remove socket file at {:?}", self.path);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let clipboard = Arc::new(Mutex::new(ServerClipboard::new()));

    // Handle socket cleanup.
    let socket_path = get_socket_path();
    let _ = fs::remove_file(&socket_path);
    let _cleanup = SocketCleanup {
        path: socket_path.clone(),
    };

    let listener = UnixListener::bind(&socket_path)?;

    loop {
        let (stream, _) = listener.accept().await?;
        let clipboard = Arc::clone(&clipboard);
        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(stream);
            let mut reader = BufReader::new(reader);

            let request_length = match reader.read_u32().await {
                Ok(length) => length,
                Err(err) => {
                    eprintln!("Failed to read request length: {}", err);
                    return;
                }
            };
            let mut request_buffer = vec![0; request_length as usize];

            if let Err(err) = reader.read_exact(&mut request_buffer).await {
                eprintln!("Error while reading: {}", err);
                return;
            }
            let response = match proto::Request::decode(&mut Cursor::new(request_buffer)) {
                Ok(request) => handle_request(request, &clipboard).await,
                Err(_) => error_response("invalid request"),
            };

            let mut response_buffer = vec![];
            if let Err(err) = response.encode(&mut response_buffer) {
                eprintln!("Error while encoding the response: {}", err);
                return;
            }

            if let Err(err) = writer.write_u32(response_buffer.len() as u32).await {
                eprintln!("Failed to write response length: {}", err);
                return;
            }

            if let Err(err) = writer.write_all(&response_buffer).await {
                eprintln!("Error while sending success message: {}", err);
            }
        });
    }
}

async fn handle_request(
    request: proto::Request,
    clipboard: &Arc<Mutex<ServerClipboard>>,
) -> proto::Response {
    let command = match proto::Command::try_from(request.command) {
        Ok(command) => command,
        Err(_) => return error_response("unknown command"),
    };

    let mut clipboard = clipboard.lock().await;
    match command {
        proto::Command::Copy | proto::Command::Cut => {
            let operation = if command == proto::Command::Copy {
                ClipboardOperation::Copy
            } else {
                ClipboardOperation::Cut
            };
            let timestamp = clipboard.clock.tick();
            clipboard.state.add_paths(
                request.files.into_iter().map(PathBuf::from),
                operation,
                timestamp,
            );
            snapshot_response(&clipboard.state)
        }
        proto::Command::Clear => {
            let timestamp = request
                .clear_timestamp
                .map(HybridTimestamp::from)
                .unwrap_or_else(|| clipboard.clock.tick());
            clipboard.clock.observe(&timestamp);
            clipboard.state.apply_clear(timestamp);
            snapshot_response(&clipboard.state)
        }
        proto::Command::GetCopy => legacy_paths_response(clipboard.state.copied_paths()),
        proto::Command::GetCut => legacy_paths_response(clipboard.state.cut_paths()),
        proto::Command::Snapshot => snapshot_response(&clipboard.state),
        proto::Command::Reconcile => {
            let items = match request
                .clipboard_items
                .into_iter()
                .map(fm::clipboard::ClipboardItem::from_proto)
                .collect::<Option<Vec<_>>>()
            {
                Some(items) => items,
                None => return error_response("invalid clipboard item"),
            };
            let clear_timestamp = request.clear_timestamp.map(HybridTimestamp::from);

            for item in &items {
                clipboard.clock.observe(&item.timestamp);
            }
            if let Some(timestamp) = &clear_timestamp {
                clipboard.clock.observe(timestamp);
            }
            clipboard.state.reconcile(items, clear_timestamp);
            snapshot_response(&clipboard.state)
        }
        proto::Command::Take => {
            let items: Vec<_> = clipboard.state.items().cloned().collect();
            let timestamp = clipboard.clock.tick();
            clipboard.state.apply_clear(timestamp.clone());
            proto::Response {
                status: "success".into(),
                files: vec![],
                clipboard_items: items.iter().map(|item| item.to_proto()).collect(),
                clear_timestamp: Some((&timestamp).into()),
            }
        }
        proto::Command::DefaultValue => error_response("unknown command"),
    }
}

fn snapshot_response(state: &ClipboardState) -> proto::Response {
    proto::Response {
        status: "success".into(),
        files: vec![],
        clipboard_items: state.items().map(|item| item.to_proto()).collect(),
        clear_timestamp: state.clear_timestamp().map(Into::into),
    }
}

fn legacy_paths_response(paths: impl IntoIterator<Item = PathBuf>) -> proto::Response {
    proto::Response {
        status: "success".into(),
        files: paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        clipboard_items: vec![],
        clear_timestamp: None,
    }
}

fn error_response(status: &str) -> proto::Response {
    proto::Response {
        status: status.into(),
        files: vec![],
        clipboard_items: vec![],
        clear_timestamp: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm::clipboard::{ClipboardItem, ClipboardOperation};

    fn timestamp(millis: u64, client_id: &str) -> HybridTimestamp {
        HybridTimestamp {
            physical_millis: millis,
            logical: 0,
            client_id: client_id.into(),
        }
    }

    fn item(path: &str, operation: ClipboardOperation, millis: u64) -> proto::ClipboardItem {
        ClipboardItem {
            path: PathBuf::from(path),
            operation,
            timestamp: timestamp(millis, "test-client"),
        }
        .to_proto()
    }

    fn request(command: proto::Command) -> proto::Request {
        proto::Request {
            command: command.into(),
            files: vec![],
            clipboard_items: vec![],
            clear_timestamp: None,
        }
    }

    #[tokio::test]
    async fn take_is_atomic_and_stale_items_cannot_return() {
        let clipboard = Arc::new(Mutex::new(ServerClipboard::new()));
        let mut reconcile = request(proto::Command::Reconcile);
        reconcile.clipboard_items = vec![
            item("/copy", ClipboardOperation::Copy, 1),
            item("/cut", ClipboardOperation::Cut, 2),
        ];
        let response = handle_request(reconcile, &clipboard).await;
        assert_eq!(response.clipboard_items.len(), 2);

        let taken = handle_request(request(proto::Command::Take), &clipboard).await;
        assert_eq!(taken.clipboard_items.len(), 2);
        assert!(taken.clear_timestamp.is_some());

        let snapshot = handle_request(request(proto::Command::Snapshot), &clipboard).await;
        assert!(snapshot.clipboard_items.is_empty());

        let mut stale = request(proto::Command::Reconcile);
        stale.clipboard_items = vec![item("/copy", ClipboardOperation::Copy, 1)];
        let response = handle_request(stale, &clipboard).await;
        assert!(response.clipboard_items.is_empty());
    }

    #[tokio::test]
    async fn newest_operation_for_a_path_wins_during_reconciliation() {
        let clipboard = Arc::new(Mutex::new(ServerClipboard::new()));
        let mut reconcile = request(proto::Command::Reconcile);
        reconcile.clipboard_items = vec![item("/same", ClipboardOperation::Cut, 1)];
        handle_request(reconcile, &clipboard).await;

        let mut reconcile = request(proto::Command::Reconcile);
        reconcile.clipboard_items = vec![item("/same", ClipboardOperation::Copy, 2)];
        let response = handle_request(reconcile, &clipboard).await;
        let item = ClipboardItem::from_proto(response.clipboard_items[0].clone()).unwrap();

        assert_eq!(item.operation, ClipboardOperation::Copy);
    }
}
