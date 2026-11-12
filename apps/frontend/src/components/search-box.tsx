import { useState } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { SearchIcon } from "lucide-react";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";

/** The search field of every signed-in page: Enter opens the results. */
export function SearchBox() {
  const navigate = useNavigate();
  const current = useRouterState({
    select: (s) =>
      s.location.pathname === "/search"
        ? String((s.location.search as { q?: string }).q ?? "")
        : "",
  });
  const [q, setQ] = useState(current);
  return (
    <form
      role="search"
      className="w-40 sm:w-64"
      onSubmit={(e) => {
        e.preventDefault();
        const query = q.trim();
        if (query) void navigate({ to: "/search", search: { q: query } });
      }}
    >
      <InputGroup>
        <InputGroupInput
          type="search"
          placeholder="Search files"
          aria-label="Search files"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <InputGroupAddon>
          <SearchIcon />
        </InputGroupAddon>
      </InputGroup>
    </form>
  );
}
