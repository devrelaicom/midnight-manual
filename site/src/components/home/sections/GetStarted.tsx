import type {ReactNode} from 'react';
import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';
import CopyButton from '../CopyButton';
import styles from '../HomeSections.module.css';

export default function GetStarted(): ReactNode {
  return (
    <section className={`${styles.section} ${styles.sectionBand}`} id="start">
      <div className={styles.container}>
        <p className={styles.marker}>Get started</p>
        <h2 className={styles.hSection}>Up and running<br />in three steps</h2>
        <p className={styles.subhead}>
          No database, no API key, no account. Install the <code>mnm</code> command, then point your
          AI client at it and ask.
        </p>

        <ol className={styles.steps}>
          {/* Step 01 */}
          <li className={styles.step}>
            <div className={styles.stepHead}>
              <span className={styles.stepN}>01</span>
              <h3 className={styles.stepTitle}>Install <code>mnm</code></h3>
            </div>
            <p className={styles.stepLead}>
              Prebuilt binaries for macOS &amp; Linux. No Rust toolchain required. (Windows runs the
              Linux build via WSL.)
            </p>
            <Tabs groupId="install">
              <TabItem value="brew" label="Homebrew" default>
                <div className={styles.cmd}>
                  <code>brew install aaronbassett/tap/midnight-manual</code>
                  <CopyButton text="brew install aaronbassett/tap/midnight-manual" />
                </div>
              </TabItem>
              <TabItem value="shell" label="Shell">
                <div className={styles.cmd}>
                  <code>{'curl --proto \'=https\' --tlsv1.2 -LsSf https://github.com/devrelaicom/midnight-manual/releases/latest/download/midnight-manual-installer.sh | sh'}</code>
                  <CopyButton text="curl --proto '=https' --tlsv1.2 -LsSf https://github.com/devrelaicom/midnight-manual/releases/latest/download/midnight-manual-installer.sh | sh" />
                </div>
              </TabItem>
              <TabItem value="source" label="From source">
                <div className={styles.cmd}>
                  <pre className={styles.cmdBlock}><code>{`git clone https://github.com/devrelaicom/midnight-manual.git
cd midnight-manual
cargo build --release -p midnight-manual
install -m 0755 target/release/mnm ~/.local/bin/mnm`}</code></pre>
                  <CopyButton text={`git clone https://github.com/devrelaicom/midnight-manual.git\ncd midnight-manual\ncargo build --release -p midnight-manual\ninstall -m 0755 target/release/mnm ~/.local/bin/mnm`} />
                </div>
                <p className={styles.stepNote}>
                  Building from source needs a{' '}
                  <a href="https://rustup.rs" rel="noopener">Rust toolchain</a> (1.91+).
                </p>
              </TabItem>
            </Tabs>
          </li>

          {/* Step 02 */}
          <li className={styles.step}>
            <div className={styles.stepHead}>
              <span className={styles.stepN}>02</span>
              <h3 className={styles.stepTitle}>Add it to your AI client</h3>
            </div>
            <p className={styles.stepLead}>
              One line wires <code>mnm</code> into your assistant as an MCP tool.
            </p>
            <Tabs groupId="client">
              <TabItem value="claude" label="Claude Code" default>
                <div className={styles.cmd}>
                  <code>claude mcp add midnight-manual -- mnm mcp serve</code>
                  <CopyButton text="claude mcp add midnight-manual -- mnm mcp serve" />
                </div>
              </TabItem>
              <TabItem value="codex" label="Codex">
                <pre className={`${styles.cmd} ${styles.cmdBlock}`}><code>{`# ~/.codex/config.toml
[mcp_servers.midnight-manual]
command = "mnm"
args = ["mcp", "serve"]`}</code></pre>
                <CopyButton text={`[mcp_servers.midnight-manual]\ncommand = "mnm"\nargs = ["mcp", "serve"]`} />
              </TabItem>
              <TabItem value="cursor" label="Cursor">
                <pre className={`${styles.cmd} ${styles.cmdBlock}`}><code>{`// ~/.cursor/mcp.json
{ "mcpServers": { "midnight-manual": { "command": "mnm", "args": ["mcp", "serve"] } } }`}</code></pre>
                <CopyButton text={`{ "mcpServers": { "midnight-manual": { "command": "mnm", "args": ["mcp", "serve"] } } }`} />
              </TabItem>
            </Tabs>
          </li>

          {/* Step 03 */}
          <li className={styles.step}>
            <div className={styles.stepHead}>
              <span className={styles.stepN}>03</span>
              <h3 className={styles.stepTitle}>Ask anything Midnight</h3>
            </div>
            <p className={styles.stepLead}>
              Restart your client and ask a Midnight question; it reaches for search and answers with
              cited sources. Prefer the terminal?
            </p>
            <div className={styles.cmd}>
              <code>{'mnm search "how do I mint a shielded token?"'}</code>
              <CopyButton text='mnm search "how do I mint a shielded token?"' />
            </div>
          </li>
        </ol>
      </div>
    </section>
  );
}
