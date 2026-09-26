// A "⋯" menu for less frequent row actions. Dangerous items go last, in red.
import {useEffect, useRef, useState, type KeyboardEvent, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {IconMore} from './icons';

export interface MenuItem {
  label: string;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
  title?: string;
}

const ITEM_HEIGHT = 34;

export function Menu({label, items, disabled, title, className, children}: {
  label: string;
  items: MenuItem[];
  disabled?: boolean;
  title?: string;
  className?: string;
  children?: ReactNode;
}) {
  const [place, setPlace] = useState<{top?: number; bottom?: number; right: number} | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const open = place !== null;

  const measure = () => {
    const rect = trigger.current?.getBoundingClientRect();
    if (!rect) return null;
    const height = items.length * ITEM_HEIGHT + 12;
    const below = rect.bottom + 4 + height <= window.innerHeight;
    return {right: Math.max(8, window.innerWidth - rect.right), ...(below ? {top: rect.bottom + 4} : {bottom: window.innerHeight - rect.top + 4})};
  };
  const show = () => setPlace(measure());

  useEffect(() => {
    if (!open) return;
    menu.current?.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')?.focus();
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!menu.current?.contains(target) && !trigger.current?.contains(target)) setPlace(null);
    };
    const hide = () => setPlace(null);
    // Scrolling moves the trigger (focusing it can scroll its table): the menu follows it.
    const follow = () => setPlace(current => current && measure());
    document.addEventListener('pointerdown', outside);
    window.addEventListener('resize', hide);
    document.addEventListener('scroll', follow, true);
    return () => {
      document.removeEventListener('pointerdown', outside);
      window.removeEventListener('resize', hide);
      document.removeEventListener('scroll', follow, true);
    };
  }, [open]);

  const keydown = (event: KeyboardEvent) => {
    const nodes = Array.from(menu.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? []);
    const index = nodes.indexOf(document.activeElement as HTMLElement);
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      setPlace(null);
      trigger.current?.focus();
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      nodes[(index + 1) % nodes.length]?.focus();
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      nodes[(index - 1 + nodes.length) % nodes.length]?.focus();
    } else if (event.key === 'Tab') {
      // Back on the trigger, so Tab carries on from there rather than from the page's end.
      setPlace(null);
      trigger.current?.focus();
    }
  };

  return <span className="menu-anchor">
    <button ref={trigger} type="button" className={className ?? 'btn-icon'} aria-label={label} title={title ?? label}
      aria-haspopup="menu" aria-expanded={open} disabled={disabled} onClick={() => (open ? setPlace(null) : show())}>
      {children ?? <IconMore/>}
    </button>
    {/* Rendered at the top of the page: a sticky table cell would otherwise trap it beneath later rows. */}
    {place && createPortal(<div ref={menu} role="menu" aria-label={label} className="menu" style={{position: 'fixed', ...place}} onKeyDown={keydown}>
      {items.map(item => <button key={item.label} type="button" role="menuitem" disabled={item.disabled} title={item.title}
        className={`menu-item${item.danger ? ' is-danger' : ''}`}
        onClick={() => {
          // Focus returns to the trigger, so a dialog opened by the item hands focus back there.
          trigger.current?.focus();
          setPlace(null);
          item.onSelect();
        }}>{item.label}</button>)}
    </div>, document.body)}
  </span>;
}
