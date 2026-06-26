import {useEffect, useRef, useState} from 'react';
import styles from './HomeSections.module.css';

const QUERY = 'how do I write a Compact contract with a sealed ledger?';
const RESULTS = [
  {path: 'midnight-docs › compact-ledger.md', score: '0.94', conf: 0.94, chips: [['recent', true], ['verified', false], ['foundation', false]]},
  {path: 'openzeppelin-compact › access/Ownable.compact', score: '0.88', conf: 0.88, chips: [['version match', false], ['partner', false]]},
  {path: 'example-kitties › src/contract.compact', score: '0.81', conf: 0.81, chips: [['community', false]]},
] as const;

export default function RetrievalDemo() {
  const ref = useRef<HTMLDivElement>(null);
  const [typed, setTyped] = useState('');
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (reduce || !('IntersectionObserver' in window)) {
      setTyped(QUERY); setLoaded(true); return;
    }
    const el = ref.current;
    if (!el) return;
    let started = false;
    const io = new IntersectionObserver((entries) => {
      if (!entries[0].isIntersecting || started) return;
      started = true; io.disconnect();
      let i = 0;
      const tick = () => {
        setTyped(QUERY.slice(0, i)); i += 1;
        if (i <= QUERY.length) setTimeout(tick, 18 + Math.random() * 26);
        else setTimeout(() => setLoaded(true), 260);
      };
      setTimeout(tick, 350);
    }, {threshold: 0.35});
    io.observe(el);
    return () => io.disconnect();
  }, []);

  return (
    <div ref={ref} className={`${styles.demo} ${loaded ? styles.demoLoaded : ''}`} aria-label="Example: a question and its cited, ranked results">
      <div className={styles.demoBar} aria-hidden="true"><i /><i /><i /><span className={styles.demoTag}>midnight-manual</span></div>
      <div className={styles.demoBody}>
        <p className={styles.demoPrompt}><span className={styles.demoSigil} aria-hidden="true">$</span> {typed}{!loaded && <span className={styles.demoCursor} aria-hidden="true" />}</p>
        <ul className={styles.demoResults}>
          {RESULTS.map((r) => (
            <li key={r.path} className={styles.demoResult} style={{['--conf' as string]: r.conf}}>
              <div className={styles.demoRhead}><span className={styles.demoPath}>{r.path}</span><span className={styles.demoScore}>{r.score}</span></div>
              <div className={styles.demoTrack} aria-hidden="true"><span className={styles.demoFill} /></div>
              <div className={styles.demoChips}>{r.chips.map(([label, accent]) => (
                <span key={label} className={`${styles.chip} ${accent ? styles.chipAccent : ''}`}>{label}</span>
              ))}</div>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
