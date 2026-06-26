import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docs: [
    { type: 'category', label: 'Get started', collapsed: false, items: ['intro', 'install', 'add-to-ai-client', 'first-search'] },
    { type: 'category', label: 'Using the MCP server', collapsed: false,
      items: ['mcp/how-it-works','mcp/searching','mcp/reading-in-context','mcp/corpus-diagnostics','mcp/advanced-search-skill','mcp/rate-limits'] },
    { type: 'category', label: 'Using the CLI', collapsed: false,
      items: ['cli/overview','cli/searching','cli/reading','cli/models','cli/configuration','cli/skills-telemetry'] },
    { type: 'category', label: 'Concepts', collapsed: false,
      items: ['concepts/confidence','concepts/hybrid-retrieval','concepts/multi-query-hyde','concepts/smart-chunker','concepts/models'] },
    { type: 'category', label: 'Self-hosting & operations', collapsed: true,
      items: ['self-hosting/when-to-self-host','self-hosting/manifests','self-hosting/ingestion-pipeline','self-hosting/running-an-ingest','self-hosting/users-access','self-hosting/versions-rate-limits','self-hosting/cloud-server'] },
    { type: 'category', label: 'Reference', collapsed: false,
      items: ['reference/mcp-tools','reference/cli','reference/configuration','reference/embeddings'] },
    'privacy',
  ],
};

export default sidebars;
