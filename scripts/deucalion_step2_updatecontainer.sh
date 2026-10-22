#!/bin/bash
#SBATCH --job-name=update-rust-rocky8
#SBATCH --account=F202316480ICDTF2X
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=1
#SBATCH --time=1:00:00
#SBATCH --partition=dev-x86

singularity exec \
  --env CARGO_HOME="$PWD/rust195/cargo" \
  --env RUSTUP_HOME="$PWD/rust195/rustup" \
  containers/rocky8-rust.sif \
  bash -lc '
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain 1.95.0 --profile minimal
  '