# nightly (fork automation only)

This branch is the default branch of bd78/lightcraft only so that GitHub runs the
scheduled `Nightly Release Build` workflow (`schedule` and `workflow_dispatch` are read
from the default branch). It holds no LightCraft code.

- `main` stays a plain copy of storytold/lightcraft `main`: never commit fork-only files there.
- Never merge this branch into `main`, `rel/*` or a feature branch.
- To change which branches go into the nightly build, edit `FEATURE_BRANCHES` in
  `.github/workflows/nightly-release.yml` on this branch.

The workflow checks out `rel/integration`, merges `upstream/main` and the feature branches,
builds the Windows release, uploads the zip, and pushes `rel/integration` after a green build.
