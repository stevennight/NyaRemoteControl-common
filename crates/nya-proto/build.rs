fn main() {
    println!("cargo:rerun-if-changed=proto");
    let fds = protox::compile(["nya.proto"], ["proto"]).expect("compile nya.proto");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate protobuf code");
}
