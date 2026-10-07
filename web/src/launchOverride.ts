let documentLaunch: { override: boolean | undefined } | undefined;

/** A launcher URL applies to this document only, never to saved preferences. */
export function consumeLaunchPlayback(fallback: () => boolean): boolean {
  if (typeof window === "undefined") return fallback();
  if (!documentLaunch) {
    const url = new URL(window.location.href);
    const value = url.searchParams.get("dsh-startup");
    documentLaunch = {
      override: value === "on" ? true : value === "off" ? false : undefined,
    };
    if (url.searchParams.has("dsh-startup")) {
      url.searchParams.delete("dsh-startup");
      window.history.replaceState(
        window.history.state,
        "",
        `${url.pathname}${url.search}${url.hash}`,
      );
    }
  }
  // Keep this lazy: an explicit invocation must not consume a separate saved
  // "next web startup" preference through the normal initialization path.
  return documentLaunch.override ?? fallback();
}
