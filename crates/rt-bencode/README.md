# rt-bencode

Canonical bencode parser and encoder.

## Status: Phase 1 — implementation complete

## Public API

```rust
// Decode
let val = rt_bencode::decode(bytes)?;

// Decode with info dict span for infohash
let (val, info_span) = rt_bencode::decode_torrent_info_span(bytes)?;

// Encode
let bytes = rt_bencode::encode(&val);

// Streaming decoder with limits
let val = rt_bencode::Decoder::new(bytes)
    .with_max_depth(32)
    .with_max_string(4 * 1024 * 1024)
    .decode()?;
```

## Acceptance criteria

- Parse integers, byte strings, lists, dicts
- Reject `-0`, leading zeros, unsorted dict keys (strict mode)
- Enforce configurable depth and string length limits
- Capture info dict byte span for exact infohash computation
- Fuzz targets in `fuzz/`
