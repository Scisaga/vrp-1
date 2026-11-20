use prost::Message;
use std::path::PathBuf;

const PROTOS: [&str; 4] = [
    "vrp/maps/v1/maps.proto",
    "vrp/engine/v1/engine.proto",
    "vrp/worker/v1/worker.proto",
    "vrp/app/v1/app.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    for file in PROTOS {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    // protox 是纯 Rust 的 proto 编译器，构建时不需要安装 protoc。
    let descriptors = protox::compile(PROTOS, [&root])?;
    let encoded = descriptors.encode_to_vec();
    prost_build::Config::new()
        // map 用 BTreeMap，保证遍历顺序确定，编译结果才可复现。
        .btree_map(["."])
        .boxed(".vrp.worker.v1.ChildCommand.command.start")
        .boxed(".vrp.worker.v1.ChildEvent.event.result")
        .boxed(".vrp.app.v1.SubmitJobRequest.source.request")
        .boxed(".vrp.app.v1.TaskInput.input.matrix")
        .boxed(".vrp.app.v1.TaskInput.input.basemap")
        .compile_fds(descriptors)?;
    pbjson_build::Builder::new()
        .register_descriptors(&encoded)?
        .btree_map(["."])
        .build(&[".vrp"])?;
    Ok(())
}
