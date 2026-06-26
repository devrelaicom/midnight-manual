import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docs: [
    { type: 'category', label: 'Get started', collapsed: false, items: ['intro', 'install', 'add-to-ai-client', 'first-search'] },
    { type: 'category', label: 'Using the MCP server', collapsed: false,
      items: ['mcp/how-it-works','mcp/searching','mcp/reading-in-context','mcp/corpus-diagnostics','mcp/advanced-search-skill','mcp/rate-limits'] },
    { type: 'category', label: 'Using the CLI', collapsed: false,
      items: ['cli/overview','cli/searching','cli/reading','cli/models','cli/configuration','cli/skills-telemetry'] },
  ],
};

export default sidebars;
