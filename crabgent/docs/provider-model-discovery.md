# Provider Model Discovery

crabgent treats provider model discovery as an input to validation, not as a
source of truth. A discovered model is only useful after a real request succeeds
on the exact provider surface that crabgent will use.

## Policy

- Discovery endpoints provide candidate model IDs.
- Runtime probes validate candidates against one surface at a time: text,
  streaming, STT, TTS, image generation, embeddings, hosted web search, or
  forced alignment.
- Probe outputs must not be committed when they contain account-local model IDs,
  credentials, response bodies, organization names, or other deployment data.
- Provider catalogs in source code should contain stable public model IDs plus
  documented capability flags.
- Consumers can still pass an explicit `ModelId` when their deployment has a
  model that is not in the static catalog.

## Probe Scripts

The scripts under `tools/` are local operator tools. They are intentionally not
part of the build:

```sh
python3 tools/probe_provider_models.py
python3 tools/probe_provider_model_capabilities.py
```

Keep probe credentials in local environment files or shell environment
variables. Do not commit raw probe artifacts.
