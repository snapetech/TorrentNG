# rt-piece-picker

Piece selection for the native engine:

- rarest-first peer selection by default
- priority pieces before normal selection, used for first/last file pieces and preview workflows
- sequential mode for client compatibility and streaming-oriented downloads
- configurable sequential start piece for Transmission-style from-piece behavior
- endgame duplicate requests after fresh work is exhausted

Priority pieces always win before sequential or rarest-first ordering. Sequential mode walks from the configured start piece to the end, then wraps to lower pieces so a torrent still completes. Endgame uses the same effective piece order as normal picking.

## Status: Implemented — native engine support
