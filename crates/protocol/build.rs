fn main() {
    println!("cargo::rerun-if-changed=../../proto/beholder/v1/daemon.proto");
    tonic_prost_build::compile_protos("../../proto/beholder/v1/daemon.proto")
        .expect("failed to compile Beholder protocol");
}
