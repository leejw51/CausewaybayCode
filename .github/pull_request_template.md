## What and why

<!-- What this changes, and why. Link the issue if there is one. -->

## Tests

- [ ] `make test`
- [ ] `make test-web` (if the web client, the protocol or the backend's answers changed)
- [ ] With the model, if the change needs it: `make test-model`, `make e2e`, `make test-playwright`

<!-- Say which you ran, and what you checked by hand. -->

## Checklist

- [ ] A change to tools, the sandbox, paths, sign-in or who the backend answers comes with a test that shows the limit holding
- [ ] The README (including Safety and Limits) is still true
- [ ] New web client text is in all eight languages in `rust/web/src/i18n.rs`
- [ ] Anything brought in from elsewhere (code, fonts, pictures) is in `THIRD_PARTY_NOTICES` with its licence
- [ ] No secrets: no `.env`, tokens, keys or recovery phrases
