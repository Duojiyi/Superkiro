// Short confirmations at the top centre of the screen. Errors that need action are shown
// where they happened instead; the toast only reports that something is done.
import {useEffect, useRef, useState} from 'react';
import {IconCheck, IconWarning} from './icons';

type Kind = 'success' | 'info' | 'error';
interface Item {id: number; kind: Kind; text: string}

let show: ((kind: Kind, text: string) => void) | null = null;

export const toast = {
  success: (text: string) => show?.('success', text),
  info: (text: string) => show?.('info', text),
  error: (text: string) => show?.('error', text),
};

export function ToastHost() {
  const [item, setItem] = useState<Item | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => {
    let next = 0;
    show = (kind, text) => {
      clearTimeout(timer.current);
      setItem({id: ++next, kind, text});
      timer.current = setTimeout(() => setItem(null), kind === 'error' ? 6000 : 3000);
    };
    return () => {show = null; clearTimeout(timer.current);};
  }, []);
  // The live region stays mounted so that each new message is announced.
  return <div className="toast-layer" role="status" aria-live="polite" data-live-layer>
    {item && <div key={item.id} className={`toast toast-${item.kind}`}>
      {item.kind === 'success' ? <IconCheck/> : item.kind === 'error' ? <IconWarning/> : null}
      <span>{item.text}</span>
    </div>}
  </div>;
}
