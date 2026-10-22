# postmesh
A post-processor written in Rust


# How to build in Deucalion

In the HPC Deucalion, to compile and build `postmesh` 3 steps are needed:
- To download a compatible docker/singularity container;
- To update the docker/singularity container with Rust 1.95;
- To build the sources.

Scripts for these tasks are provided in folder `/scripts`, just call:
1. `sbatch scripts/deucalion_step1_downloadcontainer.sh` to download the container; 
2. `sbatch scripts/deucalion_step2_updatecontainer.sh` to update the container with the Rust 1.95;
3. `sbatch scripts/deucalion_step3_build.sh` to build the sources.

NOTES: 
- the scripts must be launched in the root of the repository, i.e. using scripts/deucalion location to preserve the folder structures.
- before running the scripts update the account `#SBATCH --account=F202316480ICDTF2X`  to the one corresponding to your project.