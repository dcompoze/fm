use std::error::Error;

use prost_build::Config;

fn main() -> Result<(), Box<dyn Error>> {
    let proto_files = &["proto/server.proto"];
    let includes = &["proto/"];
    Config::default()
        .out_dir("proto")
        .compile_protos(proto_files, includes)?;
    Ok(())
}
