use std::collections::HashSet;
use std::error::Error;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::vec;

use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

#[allow(warnings)]
mod proto {
    include!("../proto/server.rs");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cut = Arc::new(Mutex::new(HashSet::<PathBuf>::new()));
    let copied = Arc::new(Mutex::new(HashSet::<PathBuf>::new()));

    let socket_path = "/tmp/fm.sock";
    std::fs::remove_file(socket_path).unwrap_or_default();

    let listener = UnixListener::bind(socket_path)?;

    loop {
        let (stream, _) = listener.accept().await?;
        let cut = Arc::clone(&cut);
        let copied = Arc::clone(&copied);
        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(stream);
            let mut reader = BufReader::new(reader);

            let request_length = reader.read_u32().await.expect("failed to read request length");
            let mut request_buffer = vec![0; request_length as usize];

            if let Err(err) = reader.read_exact(&mut request_buffer).await {
                eprintln!("Error while reading: {}", err);
                return
            }
            let response: proto::Response;
            if let Ok(request) = proto::Request::decode(&mut Cursor::new(request_buffer)) {
                if request.command == proto::Command::Copy.into() {
                    let mut list = copied.lock().await;
                    *list = HashSet::from_iter(request.files.iter().map(PathBuf::from));
                    response = proto::Response {
                        status: "success".into(),
                        files: vec![],
                    }
                } else if request.command == proto::Command::Cut.into() {
                    let mut list = cut.lock().await;
                    *list = HashSet::from_iter(request.files.iter().map(PathBuf::from));
                    response = proto::Response {
                        status: "success".into(),
                        files: vec![],
                    }
                } else if request.command == proto::Command::Clear.into() {
                    let mut list = cut.lock().await;
                    *list = HashSet::new();
                    let mut list = copied.lock().await;
                    *list = HashSet::new();
                    response = proto::Response {
                        status: "success".into(),
                        files: vec![],
                    }
                } else if request.command == proto::Command::GetCopy.into() {
                    response = proto::Response {
                        status: "success".into(),
                        files: copied
                            .lock()
                            .await
                            .clone()
                            .iter()
                            .map(|path| path.to_string_lossy().into())
                            .collect(),
                    }
                } else if request.command == proto::Command::GetCut.into() {
                    response = proto::Response {
                        status: "success".into(),
                        files: cut
                            .lock()
                            .await
                            .clone()
                            .iter()
                            .map(|path| path.to_string_lossy().into())
                            .collect(),
                    }
                } else {
                    response = proto::Response {
                        status: "unknown".into(),
                        files: vec![],
                    }
                }
            } else {
                response = proto::Response {
                    status: "error".into(),
                    files: vec![],
                }
            }

            let mut response_buffer = vec![];
            if let Err(err) = response.encode(&mut response_buffer) {
                eprintln!("Error while encoding the response: {}", err);
                return
            }

            writer
                .write_u32(response_buffer.len() as u32)
                .await
                .expect("failed to write response length");

            if let Err(err) = writer.write_all(&response_buffer).await {
                eprintln!("Error while sending success message: {}", err);
            }

            //println!("COPY: {:?}", copied);
            //println!("CUT: {:?}", cut);
        });
    }
}
