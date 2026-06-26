import type {ReactNode} from 'react';
import styles from './HomeSections.module.css';

/**
 * Static render of the retrieval demo in its final/loaded state.
 * No animation, no IntersectionObserver — Task 6 will rewrite this
 * to add the typewriter + stagger animation.
 */
export default function RetrievalDemo(): ReactNode {
  return (
    <div className={styles.demo} aria-label="Example: a question and its cited, ranked results">
      <div className={styles.demoBar} aria-hidden="true">
        <i></i><i></i><i></i>
        <span className={styles.demoTag}>midnight-manual</span>
      </div>
      <div className={styles.demoBody}>
        <p className={styles.demoPrompt}>
          <span className={styles.demoSigil} aria-hidden="true">$ </span>
          <span>how do I write a Compact contract with a sealed ledger?</span>
        </p>
        <ul className={`${styles.demoResults} ${styles.demoLoaded}`}>
          <li className={styles.demoResult} style={{'--conf': .94} as React.CSSProperties}>
            <div className={styles.demoRhead}>
              <span className={styles.demoPath}>midnight-docs › compact-ledger.md</span>
              <span className={styles.demoScore}>0.94</span>
            </div>
            <div className={styles.demoTrack} aria-hidden="true">
              <span className={styles.demoFill}></span>
            </div>
            <div className={styles.demoChips}>
              <span className={`${styles.chip} ${styles.chipAccent}`}>recent</span>
              <span className={styles.chip}>verified</span>
              <span className={styles.chip}>foundation</span>
            </div>
          </li>
          <li className={styles.demoResult} style={{'--conf': .88} as React.CSSProperties}>
            <div className={styles.demoRhead}>
              <span className={styles.demoPath}>openzeppelin-compact › access/Ownable.compact</span>
              <span className={styles.demoScore}>0.88</span>
            </div>
            <div className={styles.demoTrack} aria-hidden="true">
              <span className={styles.demoFill}></span>
            </div>
            <div className={styles.demoChips}>
              <span className={styles.chip}>version match</span>
              <span className={styles.chip}>partner</span>
            </div>
          </li>
          <li className={styles.demoResult} style={{'--conf': .81} as React.CSSProperties}>
            <div className={styles.demoRhead}>
              <span className={styles.demoPath}>example-kitties › src/contract.compact</span>
              <span className={styles.demoScore}>0.81</span>
            </div>
            <div className={styles.demoTrack} aria-hidden="true">
              <span className={styles.demoFill}></span>
            </div>
            <div className={styles.demoChips}>
              <span className={styles.chip}>community</span>
            </div>
          </li>
        </ul>
      </div>
    </div>
  );
}
