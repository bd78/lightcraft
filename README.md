# nightly (fork automation only)

This branch is the default branch of bd78/lightcraft only so that GitHub runs the
scheduled `Nightly Release Build` workflow (`schedule` and `workflow_dispatch` are read
from the default branch). It holds no LightCraft code.

- `main` stays a plain copy of storytold/lightcraft `main`: never commit fork-only files there.
- Never merge this branch into `main`, `rel/*` or a feature branch.
- To change which branches go into the nightly build, edit `FEATURE_BRANCHES` in
  `.github/workflows/nightly-release.yml` on this branch, and the table below.

The workflow checks out `rel/integration`, merges `upstream/main` and the feature branches,
builds the Windows release, uploads the zip, and pushes `rel/integration` after a green build.

`sync-main.yml` fast-forwards the fork's `main` to upstream `main` every night at 01:00
Eastern time, before the nightly build. It never force-pushes: if `main` has diverged it fails.

## Branches in the nightly build

In merge order (`FEATURE_BRANCHES`), after `upstream/main`.

| Branch | Upstream PR | What it adds or fixes |
|---|---|---|
| `export-album-var` | [#3845](https://github.com/storytold/lightcraft/pull/3845) | Export's Subfolder field expands `{album}`, `{date}` and `{hour}` at export time, with an **Insert…** menu to add them. |
| `lrc-map` | [#4243](https://github.com/storytold/lightcraft/pull/4243) | Map view: clustered photo pins, geotagging by drag and drop, location filter, saved locations, GPX track logs and an Info panel mini-map. |
| `orf-clean2` | [#3949](https://github.com/storytold/lightcraft/pull/3949) | Decodes compressed Olympus / OM System ORF and high-resolution ORI sensor data (previously only the embedded JPEG preview opened); imports `.ori`. |
| `status-box-move` | [#3953](https://github.com/storytold/lightcraft/pull/3953) | The activity stack window can be dragged anywhere on screen (kept inside the window), with a grab cursor. |
| `feat/edit-panel-camera-name` | [#4695](https://github.com/storytold/lightcraft/pull/4695) | The Edit panel names the photo's camera above the Edit header, tidied from Exif ("Olympus E-M5MarkII", "Nikon Z 8"). |
| `experiment/installed-adobe-dcp` | none (experiment, never sent upstream) | Develops raws with camera profiles read at run time from the user's profile folders, picking one made for the camera when none is chosen. |
