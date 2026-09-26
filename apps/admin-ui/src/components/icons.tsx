// Small line icons. Decorative: every button that uses one also has a text name.
import type {ReactNode} from 'react';

function Icon({children, size = 16}: {children: ReactNode; size?: number}) {
  return <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6}
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">{children}</svg>;
}

export const IconCopy = () => <Icon><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 3.2V3a1.5 1.5 0 0 0-1.5-1.5H3A1.5 1.5 0 0 0 1.5 3v6A1.5 1.5 0 0 0 3 10.5h.3"/></Icon>;
export const IconRefresh = () => <Icon><path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9"/><path d="M13.5 2.5v3h-3"/></Icon>;
export const IconMore = () => <Icon><circle cx="3.5" cy="8" r=".9" fill="currentColor"/><circle cx="8" cy="8" r=".9" fill="currentColor"/><circle cx="12.5" cy="8" r=".9" fill="currentColor"/></Icon>;
export const IconClose = () => <Icon><path d="M4 4l8 8M12 4l-8 8"/></Icon>;
export const IconChevronLeft = () => <Icon><path d="M10 3.5L5.5 8l4.5 4.5"/></Icon>;
export const IconChevronRight = () => <Icon><path d="M6 3.5L10.5 8 6 12.5"/></Icon>;
export const IconChevronDown = () => <Icon size={14}><path d="M3.5 6l4.5 4.5L12.5 6"/></Icon>;
export const IconChevronUp = () => <Icon><path d="M3.5 10L8 5.5l4.5 4.5"/></Icon>;
export const IconDoc = () => <Icon size={14}><path d="M9.5 1.5H4A1.5 1.5 0 0 0 2.5 3v10A1.5 1.5 0 0 0 4 14.5h8a1.5 1.5 0 0 0 1.5-1.5V5.5z"/><path d="M9.5 1.5v4h4M5.5 8.5h5M5.5 11h3"/></Icon>;
export const IconSearch = () => <Icon><circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/></Icon>;
export const IconCheck = () => <Icon><path d="M3 8.5l3 3 7-7"/></Icon>;
export const IconInfo = () => <Icon size={14}><circle cx="8" cy="8" r="6.2"/><path d="M8 7.2v4M8 4.9v.1"/></Icon>;
export const IconWarning = () => <Icon><path d="M8 2l6.5 11.5h-13z"/><path d="M8 6.5v3.5M8 12v.1"/></Icon>;
export const IconSortDown = () => <Icon size={12}><path d="M4 6l4 4 4-4"/></Icon>;
export const IconSortUp = () => <Icon size={12}><path d="M4 10l4-4 4 4"/></Icon>;
