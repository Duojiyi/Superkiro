// Dialogs and the side drawer. Only the topmost dialog can be used: everything beside it,
// level by level up to the app, is made inert. Focus stays inside it, Escape closes it
// (unless it is busy), and focus returns to where it was when it closes.
import {createContext, useContext, useEffect, useLayoutEffect, useRef, type ReactNode} from 'react';
import {createPortal} from 'react-dom';

/** Where dialogs are rendered: the last child of the app, beside the sidebar and workspace. */
export const ModalRootContext = createContext<HTMLElement | null>(null);

interface Entry {element: HTMLElement; escape: () => void}
const entries: Entry[] = [];
let inerted: HTMLElement[] = [];
let savedOverflow: string | null = null;

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function focusables(element: HTMLElement): HTMLElement[] {
  return Array.from(element.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(node => node.getClientRects().length > 0);
}

export const isModalOpen = () => entries.length > 0;

function isolateTopmost() {
  for (const node of inerted) node.inert = false;
  inerted = [];
  const top = entries[entries.length - 1]?.element;
  if (!top) {
    if (savedOverflow !== null) {document.body.style.overflow = savedOverflow; savedOverflow = null;}
    return;
  }
  if (savedOverflow === null) {savedOverflow = document.body.style.overflow; document.body.style.overflow = 'hidden';}
  const boundary = top.closest('.admin-app') ?? document.body;
  let node: HTMLElement = top;
  while (node !== boundary && node.parentElement) {
    for (const sibling of Array.from(node.parentElement.children) as HTMLElement[]) {
      if (sibling === node || sibling.inert || sibling.hasAttribute('data-live-layer')) continue;
      sibling.inert = true;
      inerted.push(sibling);
    }
    node = node.parentElement;
  }
}

function onKeydown(event: KeyboardEvent) {
  const top = entries[entries.length - 1];
  if (!top) return;
  if (event.key === 'Escape') {
    event.preventDefault();
    top.escape();
    return;
  }
  if (event.key !== 'Tab') return;
  const nodes = focusables(top.element);
  const first = nodes[0], last = nodes[nodes.length - 1];
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  if (!first) {event.preventDefault(); top.element.focus(); return;}
  if (!active || !top.element.contains(active)) {event.preventDefault(); first.focus(); return;}
  if (event.shiftKey && (active === first || active === top.element)) {event.preventDefault(); last.focus();}
  else if (!event.shiftKey && active === last) {event.preventDefault(); first.focus();}
}

export function Modal({label, onClose, busy, role = 'dialog', className, children}: {
  label: string;
  /** Called for Escape; ignored while busy. */
  onClose?: () => void;
  busy?: boolean;
  role?: 'dialog' | 'alertdialog';
  className?: string;
  children: ReactNode;
}) {
  const root = useContext(ModalRootContext);
  const ref = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = busy ? undefined : onClose;
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const entry: Entry = {element, escape: () => close.current?.()};
    entries.push(entry);
    if (entries.length === 1) document.addEventListener('keydown', onKeydown);
    isolateTopmost();
    (element.querySelector<HTMLElement>('[data-autofocus]:not([disabled])') ?? focusables(element)[0] ?? element).focus();
    return () => {
      const index = entries.indexOf(entry);
      if (index >= 0) entries.splice(index, 1);
      if (!entries.length) document.removeEventListener('keydown', onKeydown);
      isolateTopmost();
      if (previous?.isConnected) previous.focus();
    };
  }, [root]);
  if (!root) return null;
  return createPortal(<div className="modal-layer">
    <div className="modal-backdrop"/>
    <div ref={ref} role={role} aria-modal="true" aria-label={label} aria-busy={busy || undefined} tabIndex={-1}
      className={`modal${className ? ` ${className}` : ''}`}>{children}</div>
  </div>, root);
}

/** A panel on the right beside a list that stays usable. Escape closes it when no dialog is open. */
export function Drawer({id, label, onClose, children, className}: {id?: string; label: string; onClose: () => void; children: ReactNode; className?: string}) {
  const ref = useRef<HTMLElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    ref.current?.focus({preventScroll: true});
    const keydown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented || isModalOpen()) return;
      event.preventDefault();
      close.current();
    };
    document.addEventListener('keydown', keydown);
    return () => {
      document.removeEventListener('keydown', keydown);
      if (previous?.isConnected && !isModalOpen()) previous.focus({preventScroll: true});
    };
  }, []);
  return <aside ref={ref} id={id} aria-label={label} tabIndex={-1} className={`drawer${className ? ` ${className}` : ''}`}>{children}</aside>;
}
