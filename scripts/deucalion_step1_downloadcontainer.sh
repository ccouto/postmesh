#!/bin/bash

#SBATCH --job-name=pull-rust-rocky8
#SBATCH --account=F202316480ICDTF2X
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=4
#SBATCH --time=1:00:00
#SBATCH --partition=dev-x86

mkdir -p containers
mkdir -p singularity-cache
mkdir -p singularity-tmp

export SINGULARITY_CACHEDIR="$PWD/singularity-cache"
export SINGULARITY_TMPDIR="$PWD/singularity-tmp"

singularity pull \
    containers/rocky8-rust.sif \
    docker://jc21/rpmbuild:rocky8-rust
