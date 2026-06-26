import type {ReactNode} from 'react';
import CopyButton from '../CopyButton';
import styles from '../HomeSections.module.css';

export default function Skill(): ReactNode {
  return (
    <section className={`${styles.section} ${styles.sectionBand}`} id="skill">
      <div className={styles.container}>
        <p className={styles.marker}>It learns the technique</p>
        <h2 className={styles.hSection}>Searches like a<br />seasoned researcher</h2>
        <p className={styles.subhead}>
          The tool gives your assistant the search; the Advanced Search Skill teaches it how to use
          it well. Install once and your agent reaches for the right approach on its own.
        </p>
        <div className={`${styles.cards} ${styles.grid2}`} style={{margin: '26px 0'}}>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Rephrases the question</p>
            <p>Searches a few wordings at once, so a synonym mismatch never hides the answer.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Reads around a result</p>
            <p>Pulls the surrounding context instead of a lone snippet, so the answer holds together.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Cross-checks sources</p>
            <p>When sources disagree it surfaces the conflict rather than picking one for you.</p>
          </div>
          <div className={styles.card}>
            <p className={styles.cardTitle}>Weighs the evidence</p>
            <p>Ranks and prunes by how much each source can be trusted before it answers.</p>
          </div>
        </div>
        <p className={styles.skillCtaLabel}>
          Already have <code>mnm</code>? Ask your assistant to install the skill, or add it
          yourself:
        </p>
        <div className={styles.cmd} style={{maxWidth: '360px'}}>
          <code>mnm skills add</code>
          <CopyButton text="mnm skills add" />
        </div>
      </div>
    </section>
  );
}
