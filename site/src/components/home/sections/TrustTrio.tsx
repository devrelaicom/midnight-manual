import type {ReactNode} from 'react';
import styles from '../HomeSections.module.css';

export default function TrustTrio(): ReactNode {
  return (
    <section className={styles.section} id="trust">
      <div className={styles.container}>
        <p className={styles.marker}>Answers you can check</p>
        <h2 className={styles.hSection}>Cited and<br />ranked by trust</h2>
        <div className={`${styles.cards} ${styles.grid3}`} style={{marginTop: '26px'}}>
          <div className={styles.card}>
            <p className={styles.cardMeta}>// CITED</p>
            <p className={styles.cardTitle}>Real sources</p>
            <p>Every result points back to the exact doc or source file it came from, so your assistant can show its work instead of guessing.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardMeta}>// TRUSTED</p>
            <p className={styles.cardTitle}>Ranked by trust</p>
            <p>Results are weighted by where they come from and whether they&rsquo;re verified and up to date, so the most trustworthy source rises to the top.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardMeta}>// CURRENT</p>
            <p className={styles.cardTitle}>Always live</p>
            <p>Searches the live Midnight corpus, so answers reflect the docs as they stand today.</p>
          </div>
        </div>
      </div>
    </section>
  );
}
