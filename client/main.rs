// Unix socket client used for testing purposes only.

use std::error::Error;
use std::io::Cursor;
use std::vec;

use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[allow(warnings)]
mod proto {
    include!("../proto/server.rs");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let socket_path = "/tmp/fm.sock";

    let mut client = UnixStream::connect(socket_path).await?;

    let request = proto::Request {
        command: proto::Command::Copy.into(),
        files: vec!["/foo/bar/baz.txt".into()],
    };

    // let request = proto::Request {
    //     command: proto::Command::GetCopy.into(),
    //     files: vec![],
    // };

    let bytes = serialize_request(&request);

    client.write_u32(bytes.len() as u32).await?;
    client.write_all(&bytes).await?;

    // Read the server response.
    let response_length = client.read_u32().await?;
    let mut response_buffer = vec![0; response_length as usize];

    client.read_exact(&mut response_buffer).await?;

    let mut response_cursor = Cursor::new(response_buffer);

    if let Ok(response) = proto::Response::decode(&mut response_cursor) {
        println!("Response: {:?}", response);
    } else {
        println!("Invalid response");
    }

    Ok(())
}

pub fn serialize_request(request: &proto::Request) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(request.encoded_len());
    request.encode(&mut buffer).unwrap();
    buffer
}
