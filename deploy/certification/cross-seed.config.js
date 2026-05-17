module.exports = {
  apiKey: "certification-token-000000000000",
  host: "0.0.0.0",
  port: 2468,
  action: "inject",
  matchMode: "strict",
  useClientTorrents: true,
  torrentClients: ["qbittorrent:http://cert-token:cert-token@torrentng:8080"],
  outputDir: null,
  torznab: [],
  linkDirs: ["/downloads"],
  dataDirs: [],
  seasonFromEpisodes: null,
};
