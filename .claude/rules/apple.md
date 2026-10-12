---
paths:
  - "crates/slopty-platform/**"
  - "crates/slopty-dnd/**"
  - "crates/slopty-capture/**"
  - "crates/slopty-input/**"
  - "crates/slopty-codec/**"
  - "crates/slopty-vdisplay/**"
  - "crates/slopty-media/**"
---

# Apple code

- Floor macOS 26.5 / iOS 26.5 on Apple silicon: no availability checks, no fallbacks.
- Every `unsafe` block names the framework or ABI rule it relies on.
- Apple constants come from the objc2 statics. A `CFSTR` constant is spelled once, beside its
  use, naming its header.
- Private API is fine (internal TestFlight only): declare it once, with a test that it exists.
