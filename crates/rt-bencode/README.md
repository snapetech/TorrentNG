# rt-bencode

Canonical bencode parser and encoder.

## Status: Implemented — TorrentNG client support

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
    .with_max_nodes(250_000)
    .decode()?;
```

## Acceptance criteria

- Parse integers, byte strings, lists, dicts
- Reject `-0`, leading plus signs, leading zeros, and unsorted dict keys
  (strict mode)
- Enforce configurable depth, string length, and node-count limits
- Bound integer tokens to the supported `i64` width and length prefixes to a
  pointer-width-derived limit, with errors that do not echo oversized input
- Capture info dict byte span for exact infohash computation
- Fuzz targets in `fuzz/`
