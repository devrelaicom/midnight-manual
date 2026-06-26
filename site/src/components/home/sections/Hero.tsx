import type {ReactNode} from 'react';
import Link from '@docusaurus/Link';
import CopyButton from '../CopyButton';
import RetrievalDemo from '../RetrievalDemo';
import styles from '../HomeSections.module.css';

export default function Hero(): ReactNode {
  return (
    <section className={`${styles.section} ${styles.hero}`} id="hero">
      <div className={styles.container}>
        <div className={styles.heroTop}>
          <span>MCP · CLI</span>
          <span>Open source · Pre-production</span>
        </div>
        <div className={styles.heroHead}>
          <p className={styles.eyebrow}>Retrieval engine · Midnight Network</p>
          <h1 className={`${styles.display} ${styles.heroTitle}`}>
            Ask your docs,<br />
            <span className={styles.disclose}>
              <span className={styles.discloseBar} aria-hidden="true"></span>
              not your model
            </span>
          </h1>
        </div>
        <div className={styles.heroRow}>
          <div>
            <p className={styles.heroSub}>
              Private, current search over the real Midnight docs and source — right inside your AI
              assistant. <strong>Cited answers, not confident guesses.</strong>
            </p>
            <div className={`${styles.cmd} ${styles.heroCmd}`}>
              <code>brew install aaronbassett/tap/midnight-manual</code>
              <CopyButton text="brew install aaronbassett/tap/midnight-manual" />
            </div>
            <div className={styles.heroCta}>
              <Link
                className={`${styles.btnPill} ${styles.btnPillSolid}`}
                to="/docs/intro">
                Get started <span className={styles.btnPillArrow} aria-hidden="true">→</span>
              </Link>
              <a
                className={styles.btnPill}
                href="https://github.com/devrelaicom/midnight-manual"
                rel="noopener">
                View on GitHub <span aria-hidden="true">↗</span>
              </a>
            </div>
          </div>
          <RetrievalDemo />
        </div>
      </div>
    </section>
  );
}
