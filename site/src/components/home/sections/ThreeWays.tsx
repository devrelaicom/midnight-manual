import type {ReactNode} from 'react';
import styles from '../HomeSections.module.css';

export default function ThreeWays(): ReactNode {
  return (
    <section className={styles.section} id="ways">
      <div className={styles.container}>
        <p className={styles.marker}>One install, three ways in</p>
        <h2 className={styles.hSection}>However you<br />like to work</h2>
        <div className={`${styles.cards} ${styles.grid3}`} style={{marginTop: '26px'}}>
          <div className={styles.card}>
            <p className={styles.cardMeta}>// IN YOUR AI CLIENT</p>
            <p className={styles.cardTitle}>As an MCP tool</p>
            <p>Drop one command into Claude Code, Codex, or Cursor — your assistant gains grounded search, navigation, and citations.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardMeta}>// IN YOUR TERMINAL</p>
            <p className={styles.cardTitle}>As a CLI</p>
            <p>Search the corpus, read results in context, and manage settings with <code>mnm</code>. Add <code>--json</code> to anything to script it.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardMeta}>// HOSTED FOR YOU</p>
            <p className={styles.cardTitle}>No server to run</p>
            <p>The indexed corpus is hosted by default — most people never run a server. It&rsquo;s built in, so search works the moment you install.</p>
          </div>
        </div>
      </div>
    </section>
  );
}
