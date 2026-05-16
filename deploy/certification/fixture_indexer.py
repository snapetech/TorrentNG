#!/usr/bin/env python3
import html
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse


PORT = int(os.environ.get("FIXTURE_INDEXER_PORT", "8082"))
TORRENT_PATH = os.environ.get("FIXTURE_TORRENT_PATH", "/downloads/cert-fixture/rtng-fixture.torrent")
PUBLIC_BASE = os.environ.get("FIXTURE_PUBLIC_BASE", f"http://localhost:{PORT}")
TITLE = os.environ.get("FIXTURE_TITLE", "rtng-fixture.bin")
GUID = os.environ.get("FIXTURE_GUID", "rtng-fixture-guid")
SIZE = os.environ.get("FIXTURE_SIZE", "1048576")


def xml_response(body: str) -> bytes:
    return f'<?xml version="1.0" encoding="UTF-8"?>\n{body}'.encode()


class Handler(BaseHTTPRequestHandler):
    server_version = "rtng-fixture-indexer/1.0"

    def log_message(self, fmt, *args):
        if os.environ.get("FIXTURE_LOG_REQUESTS") == "1":
            print(f"{self.address_string()} {self.command} {self.path}", file=sys.stderr, flush=True)

    def send_bytes(self, status: int, content_type: str, body: bytes):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/download/rtng-fixture.torrent":
            try:
                with open(TORRENT_PATH, "rb") as torrent:
                    body = torrent.read()
            except FileNotFoundError:
                self.send_bytes(404, "text/plain", b"fixture torrent not found")
                return
            self.send_bytes(200, "application/x-bittorrent", body)
            return

        if parsed.path != "/api":
            self.send_bytes(404, "text/plain", b"not found")
            return

        query = parse_qs(parsed.query)
        mode = query.get("t", ["search"])[0]
        if mode == "get":
            try:
                with open(TORRENT_PATH, "rb") as torrent:
                    body = torrent.read()
            except FileNotFoundError:
                self.send_bytes(404, "text/plain", b"fixture torrent not found")
                return
            self.send_bytes(200, "application/x-bittorrent", body)
            return

        if mode == "caps":
            body = """
<caps>
  <server title="rtorrentNG Fixture Torznab" version="1.0"/>
  <limits max="100" default="100"/>
  <searching>
    <rss available="yes" supportedParams=""/>
    <search available="yes" supportedParams="q"/>
    <tv-search available="yes" supportedParams="q,season,ep,rid,tvdbid,imdbid"/>
    <movie-search available="yes" supportedParams="q,imdbid,tmdbid"/>
  </searching>
  <categories>
    <category id="5000" name="TV"/>
    <category id="5030" name="TV/SD"/>
    <category id="5040" name="TV/HD"/>
    <category id="2000" name="Movies"/>
    <category id="2010" name="Movies/Foreign"/>
    <category id="2040" name="Movies/HD"/>
  </categories>
</caps>
"""
            self.send_bytes(200, "application/xml", xml_response(body))
            return

        escaped_title = html.escape(TITLE)
        escaped_guid = html.escape(GUID)
        raw_torrent_url = f"{PUBLIC_BASE}/api?t=get&id={GUID}&apikey=fixture"
        torrent_url = html.escape(raw_torrent_url, quote=True)
        comments_url = html.escape(f"{PUBLIC_BASE}/download/rtng-fixture.torrent", quote=True)
        body = f"""
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>rtorrentNG Fixture Torznab</title>
    <item>
      <title>{escaped_title}</title>
      <guid isPermaLink="false">{escaped_guid}</guid>
      <link>{torrent_url}</link>
      <comments>{comments_url}</comments>
      <pubDate>Sat, 16 May 2026 00:00:00 GMT</pubDate>
      <size>{SIZE}</size>
      <enclosure url="{torrent_url}" length="{SIZE}" type="application/x-bittorrent"/>
      <torznab:attr name="category" value="5000"/>
      <torznab:attr name="seeders" value="1"/>
      <torznab:attr name="peers" value="1"/>
      <torznab:attr name="downloadvolumefactor" value="0"/>
      <torznab:attr name="uploadvolumefactor" value="1"/>
    </item>
  </channel>
</rss>
"""
        self.send_bytes(200, "application/xml", xml_response(body))


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
