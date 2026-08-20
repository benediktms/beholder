fn main() {
    println!("cargo::rerun-if-changed=../../proto/beholder/v1/daemon.proto");
    println!("cargo::rerun-if-changed=../../proto/beholder/worker/v1/worker.proto");
    tonic_prost_build::configure()
        .compile_protos(
            &[
                "../../proto/beholder/v1/daemon.proto",
                "../../proto/beholder/worker/v1/worker.proto",
            ],
            &["../../proto"],
        )
        .expect("failed to compile Beholder protocols");
}
