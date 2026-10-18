import { describe, expect, it } from "vitest";
import {
  DEFAULT_SCOPES,
  acceptErrorText,
  firewallText,
  formatCountdown,
  initialLinkUi,
  interfaceLabel,
  isExpired,
  linkReducer,
  loopbackOnly,
  makeSerial,
  needsFirewallNotice,
  parsePort,
  qrDataUrl,
  sameSet,
  secondsLeft,
  statusPill,
  toggleScope,
  truncateName,
} from "./link";
import type { LinkStatus, PendingPairing } from "./types";

const status = (over: Partial<LinkStatus> = {}): LinkStatus => ({
  available: true,
  os: "windows",
  enabled: false,
  running: false,
  addrs: [],
  port: 47800,
  binds: ["127.0.0.1"],
  missing: [],
  error: null,
  config_readonly: null,
  firewall_notice_seen: false,
  ...over,
});

const request = (id: string): PendingPairing => ({ id, client_name: "Pixel", fingerprint: "aaaa-bbbb", expires_at: 10_000 });

describe("scopes", () => {
  it("a new device gets read and answer", () => {
    expect(DEFAULT_SCOPES).toEqual(["read", "answer"]);
  });

  it("toggling adds and removes, in the fixed order", () => {
    expect(toggleScope(["read", "answer"], "land")).toEqual(["read", "answer", "land"]);
    expect(toggleScope(["read", "answer"], "control")).toEqual(["read", "answer", "control"]);
    expect(toggleScope(["answer", "read"], "answer")).toEqual(["read"]);
    expect(toggleScope(["land", "read"], "control")).toEqual(["read", "control", "land"]);
  });

  it("never removes the last scope", () => {
    expect(toggleScope(["read"], "read")).toEqual(["read"]);
    expect(toggleScope(["land"], "land")).toEqual(["land"]);
  });

  it("does not change the list it was given", () => {
    const before: ("read" | "answer")[] = ["read", "answer"];
    toggleScope(before, "answer");
    expect(before).toEqual(["read", "answer"]);
  });
});

describe("needsFirewallNotice", () => {
  it("asks for a first enable that goes beyond this computer", () => {
    expect(needsFirewallNotice({ enabled: true, binds: ["192.168.1.20"] }, false)).toBe(true);
    expect(needsFirewallNotice({ enabled: true, binds: ["127.0.0.1", "100.101.102.103"] }, false)).toBe(true);
  });

  it("does not ask for loopback only, for an off link, or once seen", () => {
    expect(needsFirewallNotice({ enabled: true, binds: ["127.0.0.1"] }, false)).toBe(false);
    expect(needsFirewallNotice({ enabled: true, binds: [] }, false)).toBe(false);
    expect(needsFirewallNotice({ enabled: false, binds: ["192.168.1.20"] }, false)).toBe(false);
    expect(needsFirewallNotice({ enabled: true, binds: ["192.168.1.20"] }, true)).toBe(false);
  });

  it("words the notice for each OS and does not promise a network profile for Tailscale", () => {
    expect(firewallText("windows", 47800)).toContain("Private networks");
    expect(firewallText("windows", 47800)).toContain("Cancel");
    expect(firewallText("macos", 47800)).toContain("Allow");
    expect(firewallText("linux", 50123)).toContain("TCP port 50123");
    expect(firewallText("windows", 47800)).not.toContain("Tailscale");
  });
});

describe("addresses and the port", () => {
  it("labels loopback, Tailscale and the local network", () => {
    expect(interfaceLabel({ ip: "127.0.0.1", kind: "loopback", name: "This computer only" })).toBe(
      "127.0.0.1 - this computer only",
    );
    expect(interfaceLabel({ ip: "100.101.102.103", kind: "tailscale", name: "Tailscale" })).toBe(
      "100.101.102.103 - Tailscale",
    );
    expect(interfaceLabel({ ip: "192.168.1.20", kind: "lan", name: "Wi-Fi" })).toBe(
      "192.168.1.20 - local network (Wi-Fi)",
    );
    expect(interfaceLabel({ ip: "8.8.8.8", kind: "other", name: "Eth" })).toBe("8.8.8.8 - other (Eth)");
  });

  it("accepts a port from 1024 to 65535 and nothing else", () => {
    expect(parsePort("47800")).toBe(47800);
    expect(parsePort(" 1024 ")).toBe(1024);
    expect(parsePort("65535")).toBe(65535);
    for (const bad of ["", "0", "80", "1023", "65536", "99999", "-1", "47.8", "4e3", "abc", "123456"]) {
      expect(parsePort(bad), bad).toBeNull();
    }
  });

  it("compares address sets without caring about order", () => {
    expect(sameSet(["a", "b"], ["b", "a"])).toBe(true);
    expect(sameSet(["a"], ["a", "b"])).toBe(false);
  });

  it("knows a loopback-only offer", () => {
    expect(loopbackOnly(["127.0.0.1:47800"])).toBe(true);
    expect(loopbackOnly(["127.0.0.1:47800", "192.168.1.20:47800"])).toBe(false);
    expect(loopbackOnly([])).toBe(false);
  });
});

describe("statusPill", () => {
  it("says what the link is doing", () => {
    expect(statusPill(null).text).toBe("Link");
    expect(statusPill(status())).toEqual({ text: "Link off", tone: "off" });
    expect(statusPill(status({ running: true, addrs: ["127.0.0.1:47800"] }))).toEqual({
      text: "Link on - 1 address",
      tone: "on",
    });
    expect(statusPill(status({ running: true, addrs: ["a:1", "b:1"] })).text).toBe("Link on - 2 addresses");
    expect(statusPill(status({ enabled: true, error: "Port 47800 is already in use." }))).toEqual({
      text: "Link error",
      tone: "error",
    });
    expect(statusPill(status({ available: false })).tone).toBe("error");
  });
});

describe("time", () => {
  it("counts down to the expiry and stops at zero", () => {
    expect(secondsLeft(300_000, 0)).toBe(300);
    expect(secondsLeft(300_000, 1)).toBe(300);
    expect(secondsLeft(300_000, 299_001)).toBe(1);
    expect(secondsLeft(300_000, 300_000)).toBe(0);
    expect(secondsLeft(300_000, 400_000)).toBe(0);
  });

  it("is expired at the expiry time and after it", () => {
    expect(isExpired(1000, 999)).toBe(false);
    expect(isExpired(1000, 1000)).toBe(true);
    expect(isExpired(1000, 5000)).toBe(true);
  });

  it("formats minutes and seconds", () => {
    expect(formatCountdown(299)).toBe("4:59");
    expect(formatCountdown(300)).toBe("5:00");
    expect(formatCountdown(61)).toBe("1:01");
    expect(formatCountdown(0)).toBe("0:00");
    expect(formatCountdown(-5)).toBe("0:00");
  });
});

describe("truncateName", () => {
  it("leaves a short name and cuts a long one with an ellipsis", () => {
    expect(truncateName("Pixel 9")).toBe("Pixel 9");
    const cut = truncateName("x".repeat(200));
    expect(Array.from(cut)).toHaveLength(64);
    expect(cut.endsWith("…")).toBe(true);
  });

  it("does not split a surrogate pair", () => {
    const cut = truncateName("\u{1F600}".repeat(100), 10);
    expect(Array.from(cut)).toHaveLength(10);
    expect(cut).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
  });
});

describe("acceptErrorText", () => {
  it("explains a request the phone stopped waiting for", () => {
    expect(acceptErrorText("no such pairing request")).toBe("Your phone stopped waiting. Open pairing again.");
  });

  it("explains a full device list", () => {
    expect(acceptErrorText("too many paired clients")).toBe("32 devices are paired. Remove one first.");
  });

  it("passes any other message through, and never shows an empty one", () => {
    expect(acceptErrorText("clients file is not valid")).toBe("clients file is not valid");
    expect(acceptErrorText("")).toBe("Could not pair this device.");
  });
});

describe("qrDataUrl", () => {
  it("is a data URL an <img> can show, with the markup escaped", () => {
    const svg = '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1"/></svg>';
    const url = qrDataUrl(svg);
    expect(url.startsWith("data:image/svg+xml,")).toBe(true);
    expect(url).not.toContain("<");
    expect(url).not.toContain('"');
    expect(decodeURIComponent(url.slice("data:image/svg+xml,".length))).toBe(svg);
  });
});

describe("makeSerial", () => {
  it("runs calls in the order they were made even when an earlier one is slower", async () => {
    const serial = makeSerial();
    const order: string[] = [];
    const slow = serial(async () => {
      await new Promise((r) => setTimeout(r, 20));
      order.push("open 1");
    });
    const fast = serial(async () => {
      order.push("close 1");
    });
    const last = serial(async () => {
      order.push("open 2");
      return 7;
    });
    await Promise.all([slow, fast]);
    expect(await last).toBe(7);
    expect(order).toEqual(["open 1", "close 1", "open 2"]);
  });

  it("keeps going after a call fails", async () => {
    const serial = makeSerial();
    await expect(serial(async () => Promise.reject(new Error("boom")))).rejects.toThrow("boom");
    expect(await serial(async () => "ok")).toBe("ok");
  });
});

describe("linkReducer", () => {
  it("switches between the work view and the devices view", () => {
    const s = linkReducer(initialLinkUi, { type: "show", view: "devices" });
    expect(s.view).toBe("devices");
    expect(linkReducer(s, { type: "show", view: "work" }).view).toBe("work");
  });

  it("closing the devices view closes the QR dialog", () => {
    let s = linkReducer(initialLinkUi, { type: "status", status: status({ running: true, addrs: ["127.0.0.1:47800"] }) });
    s = linkReducer(linkReducer(s, { type: "show", view: "devices" }), { type: "pairing", open: true });
    expect(s.pairing).toBe(true);
    expect(linkReducer(s, { type: "show", view: "work" }).pairing).toBe(false);
  });

  it("a request closes the QR dialog and is queued once", () => {
    let s = linkReducer(initialLinkUi, { type: "pairing", open: true });
    expect(s.pairing).toBe(true);
    s = linkReducer(s, { type: "request", request: request("p1") });
    expect(s.pairing).toBe(false);
    s = linkReducer(s, { type: "request", request: request("p1") });
    s = linkReducer(s, { type: "request", request: request("p2") });
    expect(s.requests.map((r) => r.id)).toEqual(["p1", "p2"]);
  });

  it("will not open the QR dialog while a request waits", () => {
    const s = linkReducer(initialLinkUi, { type: "request", request: request("p1") });
    expect(linkReducer(s, { type: "pairing", open: true }).pairing).toBe(false);
  });

  it("answering removes only that request", () => {
    let s = linkReducer(initialLinkUi, { type: "requests-loaded", requests: [request("p1"), request("p2")] });
    s = linkReducer(s, { type: "answered", id: "p1" });
    expect(s.requests.map((r) => r.id)).toEqual(["p2"]);
    expect(linkReducer(s, { type: "answered", id: "zz" }).requests).toHaveLength(1);
  });

  it("a link that stopped drops its requests and closes the QR dialog", () => {
    let s = linkReducer(initialLinkUi, { type: "status", status: status({ running: true }) });
    s = linkReducer(s, { type: "request", request: request("p1") });
    expect(s.requests).toHaveLength(1);
    s = linkReducer(s, { type: "status", status: status({ running: false }) });
    expect(s.requests).toHaveLength(0);
    expect(s.status?.running).toBe(false);
  });

  it("keeps at most 16 requests, newest last", () => {
    let s = initialLinkUi;
    for (let i = 0; i < 20; i++) s = linkReducer(s, { type: "request", request: request(`p${i}`) });
    expect(s.requests).toHaveLength(16);
    expect(s.requests[15].id).toBe("p19");
  });

  it("a status event that arrived first beats the startup snapshot", () => {
    let s = linkReducer(initialLinkUi, { type: "status", status: status({ running: true }) });
    s = linkReducer(s, { type: "requests-loaded", requests: [request("p1")] });
    s = linkReducer(s, { type: "snapshot", status: status({ running: false }) });
    expect(s.status?.running).toBe(true);
    expect(s.requests).toHaveLength(1);
  });

  it("the startup snapshot applies when no event came first", () => {
    const first = status({ running: true });
    let s = linkReducer(initialLinkUi, { type: "snapshot", status: first });
    expect(s.status).toEqual(first);
    expect(s.statusSeen).toBe(false);
    const later = status({ running: false });
    s = linkReducer(s, { type: "status", status: later });
    expect(s.status).toEqual(later);
    expect(s.statusSeen).toBe(true);
  });

  it("a set_config result counts as seen", () => {
    const saved = status({ enabled: true });
    let s = linkReducer(initialLinkUi, { type: "status", status: saved });
    s = linkReducer(s, { type: "snapshot", status: status({ enabled: false }) });
    expect(s.status).toEqual(saved);
  });
});
