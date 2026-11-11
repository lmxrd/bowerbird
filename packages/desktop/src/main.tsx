import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "next-themes";
import { client } from "organizer-sdk/client";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { queryClient } from "@/lib/api";
import { App } from "./app";
import { appFetch } from "./lib/transport";
import "./index.css";

// The SDK's requests go through the app, with the device's token.
client.setConfig({ fetch: appFetch });

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {/* Light or dark as the system is: shadcn's `.dark` class. */}
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem>
      <QueryClientProvider client={queryClient}>
        <TooltipProvider>
          <App />
          <Toaster />
        </TooltipProvider>
      </QueryClientProvider>
    </ThemeProvider>
  </StrictMode>,
);
