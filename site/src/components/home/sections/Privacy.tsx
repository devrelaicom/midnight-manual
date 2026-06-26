import type {ReactNode} from 'react';
import styles from '../HomeSections.module.css';

export default function Privacy(): ReactNode {
  return (
    <section className={styles.section} id="private">
      <div className={styles.container}>
        <p className={styles.marker}>Private by design</p>
        <h2 className={styles.hSection}>Your questions<br />stay yours</h2>
        <div className={`${styles.cards} ${styles.grid3}`} style={{marginTop: '26px'}}>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Never logged</p>
            <p>Searching runs against the hosted corpus, which records only counts — never your query text or the passages you read.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Opt out anytime</p>
            <p>Anonymous usage stats carry no queries, content, or paths. Turn them off with an env var, a config flag, or <code>mnm telemetry disable</code>.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Leak-tested</p>
            <p>A continuous test pushes fake secrets through every path that touches your text — any leak fails the build before it ships.</p>
          </div>
        </div>
      </div>
    </section>
  );
}
