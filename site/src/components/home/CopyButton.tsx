import {useState, useCallback} from 'react';
import styles from './HomeSections.module.css';

export default function CopyButton({text}: {text: string}) {
  const [copied, setCopied] = useState(false);
  const onClick = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch { /* clipboard blocked; no-op */ }
  }, [text]);
  return (
    <button className={styles.cmdCopy} data-copied={copied || undefined} onClick={onClick}>
      {copied ? 'Copied' : 'Copy'}
    </button>
  );
}
