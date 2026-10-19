#!/bin/bash
#SBATCH --job-name=build-postmesh
#SBATCH --account=F202316480ICDTF2X
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=4
#SBATCH --time=1:00:00
#SBATCH --partition=dev-x86

rm -rf target
mkdir -p .cargo-home

singularity exec \
  --env CARGO_HOME="$PWD/.cargo-home" \
  --env RUSTUP_HOME="$PWD/rust195/rustup" \
  --env PATH="$PWD/rust195/cargo/bin:/usr/bin:/bin" \
  containers/rocky8-rust.sif \
  cargo build --release