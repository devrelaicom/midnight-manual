import type {ReactNode} from 'react';
import styles from '../HomeSections.module.css';

export default function Why(): ReactNode {
  return (
    <section className={`${styles.section} ${styles.sectionBand}`} id="why">
      <div className={styles.container}>
        <p className={styles.lead}>
          Your AI assistant&rsquo;s training data went stale the day it shipped.{' '}
          <strong>midnight-manual</strong> gives it live, ranked search over the real Midnight docs
          and source, so every answer traces back to something you can check.
        </p>
      </div>
    </section>
  );
}
