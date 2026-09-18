import type { KeyboardEvent, ReactNode } from 'react';
import { useNavigate } from 'react-router-dom';
import { cn } from '@/lib/cn';

/**
 * One table shell for every tabular surface, so the activity log and the block
 * list share column rhythm, header weight and row affordances instead of each
 * inventing their own.
 */
export interface Column {
  key: string;
  header: ReactNode;
  align?: 'left' | 'right';
  /** Hidden below the `sm` breakpoint. */
  collapse?: boolean;
  width?: string;
}

export function DataTable({ columns, children }: { columns: Column[]; children: ReactNode }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[520px] text-[13px]">
        <thead>
          <tr className="border-line-strong bg-raised sticky top-0 z-10 border-b-2">
            {columns.map((column) => (
              <th
                key={column.key}
                scope="col"
                style={column.width ? { width: column.width } : undefined}
                className={cn(
                  'text-ink px-5 py-3 text-[11px] font-bold tracking-[0.12em] uppercase',
                  column.align === 'right' ? 'text-right' : 'text-left',
                  column.collapse && 'max-sm:hidden',
                )}
              >
                {column.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

export function Row({
  children,
  index = 0,
  to,
  className,
}: {
  children: ReactNode;
  index?: number;
  /** Makes the whole row a navigation target, not just the link inside it. */
  to?: string;
  className?: string;
}) {
  const navigate = useNavigate();

  return (
    <tr
      style={{ animationDelay: `${Math.min(index, 8) * 30}ms` }}
      onClick={to ? () => void navigate(to) : undefined}
      // A row-wide click target must also be reachable from the keyboard.
      // The cell link remains the primary control; this makes the row itself
      // focusable and activatable rather than mouse-only.
      {...(to
        ? {
            tabIndex: 0,
            role: 'link' as const,
            onKeyDown: (event: KeyboardEvent<HTMLTableRowElement>) => {
              if (event.key !== 'Enter' && event.key !== ' ') return;
              if (event.target !== event.currentTarget) return;
              event.preventDefault();
              void navigate(to);
            },
          }
        : {})}
      className={cn(
        'border-line hover:bg-accent-soft animate-rise border-b transition-colors last:border-b-0',
        '[&>td]:px-5 [&>td]:py-3.5 [&>td]:align-middle',
        to &&
          'focus-visible:outline-accent cursor-pointer focus-visible:outline-2 focus-visible:-outline-offset-2',
        className,
      )}
    >
      {children}
    </tr>
  );
}

/** Right-aligned numerics use tabular figures so columns line up per digit. */
export function NumCell({ children, className }: { children: ReactNode; className?: string }) {
  return <td className={cn('text-right tabular-nums', className)}>{children}</td>;
}
