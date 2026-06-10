import {
  CalendarClock,
  LayoutDashboard,
  Settings,
  SquareCheckBig,
  Workflow,
  type LucideIcon,
} from "lucide-react";
import { useStore, type Section } from "../store";

/**
 * The 48px icon rail — section navigation only. No agents item (agents are
 * reached through the sidebar / palette / dashboard), no bell, no profile, no
 * brand mark. Active = 2px accent pip + raised bg. All clicks route through
 * setSection; navigation is instant (no context-switch gate).
 */
const ITEMS: {
  id: Section;
  icon: LucideIcon;
  label: string;
  groupBreak?: boolean;
}[] = [
  { id: "dashboard", icon: LayoutDashboard, label: "Dashboard" },
  { id: "tasks", icon: SquareCheckBig, label: "Tasks" },
  { id: "workflows", icon: Workflow, label: "Workflows", groupBreak: true },
  { id: "schedules", icon: CalendarClock, label: "Schedules" },
];

export function Rail() {
  const section = useStore((s) => s.section);
  const setSection = useStore((s) => s.setSection);

  return (
    <nav
      aria-label="Sections"
      className="flex w-12 shrink-0 flex-col items-center border-r border-hairline bg-ink-800 py-2"
    >
      <div className="flex flex-1 flex-col items-center gap-0.5 px-2 pt-1">
        {ITEMS.map((item) => (
          <div key={item.id} className="contents">
            {item.groupBreak && <div className="my-1.5 h-px w-5 bg-ink-600" />}
            <RailButton
              icon={item.icon}
              label={item.label}
              active={section === item.id}
              onClick={() => setSection(item.id)}
            />
          </div>
        ))}
      </div>
      <div className="px-2 pb-1">
        <RailButton
          icon={Settings}
          label="Settings"
          size={16}
          active={section === "settings"}
          onClick={() => setSection("settings")}
        />
      </div>
    </nav>
  );
}

function RailButton({
  icon: Icon,
  label,
  active,
  onClick,
  size = 17,
}: {
  icon: LucideIcon;
  label: string;
  active: boolean;
  onClick: () => void;
  size?: number;
}) {
  return (
    <button
      onClick={onClick}
      title={label}
      aria-label={label}
      aria-current={active ? "page" : undefined}
      className={`relative flex h-8 w-8 items-center justify-center rounded-md ${
        active
          ? "bg-ink-500 text-zinc-100"
          : "text-zinc-600 hover:bg-ink-600 hover:text-zinc-300"
      }`}
    >
      {/* The -left-2 pip is coupled to the rail's px-2 padding. */}
      {active && (
        <span className="absolute -left-2 h-[18px] w-[2px] rounded-r bg-accent" />
      )}
      <Icon size={size} />
    </button>
  );
}
