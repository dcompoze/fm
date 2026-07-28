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
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, Mutex};

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
    let (clipboard_changes, _) = broadcast::channel(32);

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
        let clipboard_changes = clipboard_changes.clone();
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
            let request = match proto::Request::decode(&mut Cursor::new(request_buffer)) {
                Ok(request) => request,
                Err(_) => {
                    let _ = write_response(&mut writer, &error_response("invalid request")).await;
                    return;
                }
            };

            if proto::Command::try_from(request.command) == Ok(proto::Command::Subscribe) {
                serve_subscription(&mut writer, clipboard_changes.subscribe()).await;
                return;
            }

            let response = handle_request(request, &clipboard, &clipboard_changes).await;
            if let Err(err) = write_response(&mut writer, &response).await {
                eprintln!("Error while sending response: {}", err);
            }
        });
    }
}

async fn handle_request(
    request: proto::Request,
    clipboard: &Arc<Mutex<ServerClipboard>>,
    clipboard_changes: &broadcast::Sender<()>,
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
            let _ = clipboard_changes.send(());
            snapshot_response(&clipboard.state)
        }
        proto::Command::Clear => {
            let timestamp = request
                .clear_timestamp
                .map(HybridTimestamp::from)
                .unwrap_or_else(|| clipboard.clock.tick());
            clipboard.clock.observe(&timestamp);
            clipboard.state.apply_clear(timestamp);
            let _ = clipboard_changes.send(());
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
            let _ = clipboard_changes.send(());
            snapshot_response(&clipboard.state)
        }
        proto::Command::Take => {
            let items: Vec<_> = clipboard.state.items().cloned().collect();
            let timestamp = clipboard.clock.tick();
            clipboard.state.apply_clear(timestamp.clone());
            let _ = clipboard_changes.send(());
            proto::Response {
                status: "success".into(),
                files: vec![],
                clipboard_items: items.iter().map(|item| item.to_proto()).collect(),
                clear_timestamp: Some((&timestamp).into()),
            }
        }
        proto::Command::DefaultValue | proto::Command::Subscribe => {
            error_response("unknown command")
        }
    }
}

async fn serve_subscription<W>(writer: &mut W, mut changes: broadcast::Receiver<()>)
where
    W: AsyncWrite + Unpin,
{
    if write_response(writer, &success_response()).await.is_err() {
        return;
    }

    loop {
        match changes.recv().await {
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                if write_response(writer, &success_response()).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn write_response<W>(writer: &mut W, response: &proto::Response) -> Result<(), Box<dyn Error>>
where
    W: AsyncWrite + Unpin,
{
    let mut response_buffer = vec![];
    response.encode(&mut response_buffer)?;
    writer.write_u32(response_buffer.len() as u32).await?;
    writer.write_all(&response_buffer).await?;
    Ok(())
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

fn success_response() -> proto::Response {
    proto::Response {
        status: "success".into(),
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
        let (changes, _) = broadcast::channel(8);
        let mut reconcile = request(proto::Command::Reconcile);
        reconcile.clipboard_items = vec![
            item("/copy", ClipboardOperation::Copy, 1),
            item("/cut", ClipboardOperation::Cut, 2),
        ];
        let response = handle_request(reconcile, &clipboard, &changes).await;
        assert_eq!(response.clipboard_items.len(), 2);

        let taken = handle_request(request(proto::Command::Take), &clipboard, &changes).await;
        assert_eq!(taken.clipboard_items.len(), 2);
        assert!(taken.clear_timestamp.is_some());

        let snapshot =
            handle_request(request(proto::Command::Snapshot), &clipboard, &changes).await;
        assert!(snapshot.clipboard_items.is_empty());

        let mut stale = request(proto::Command::Reconcile);
        stale.clipboard_items = vec![item("/copy", ClipboardOperation::Copy, 1)];
        let response = handle_request(stale, &clipboard, &changes).await;
        assert!(response.clipboard_items.is_empty());
    }

    #[tokio::test]
    async fn newest_operation_for_a_path_wins_during_reconciliation() {
        let clipboard = Arc::new(Mutex::new(ServerClipboard::new()));
        let (changes, mut change_notifications) = broadcast::channel(8);
        let mut reconcile = request(proto::Command::Reconcile);
        reconcile.clipboard_items = vec![item("/same", ClipboardOperation::Cut, 1)];
        handle_request(reconcile, &clipboard, &changes).await;
        assert_eq!(change_notifications.try_recv(), Ok(()));

        let mut reconcile = request(proto::Command::Reconcile);
        reconcile.clipboard_items = vec![item("/same", ClipboardOperation::Copy, 2)];
        let response = handle_request(reconcile, &clipboard, &changes).await;
        let item = ClipboardItem::from_proto(response.clipboard_items[0].clone()).unwrap();

        assert_eq!(item.operation, ClipboardOperation::Copy);
    }

    #[tokio::test]
    async fn subscription_receives_clipboard_change_notifications() {
        let (changes, receiver) = broadcast::channel(8);
        let (mut client, mut server) = tokio::io::duplex(1024);
        let subscription =
            tokio::spawn(async move { serve_subscription(&mut server, receiver).await });

        let acknowledgement_length = client.read_u32().await.unwrap();
        let mut acknowledgement = vec![0; acknowledgement_length as usize];
        client.read_exact(&mut acknowledgement).await.unwrap();

        changes.send(()).unwrap();
        let notification_length = client.read_u32().await.unwrap();
        let mut notification = vec![0; notification_length as usize];
        client.read_exact(&mut notification).await.unwrap();
        let response = proto::Response::decode(&mut Cursor::new(notification)).unwrap();

        assert_eq!(response.status, "success");
        subscription.abort();
    }
}
