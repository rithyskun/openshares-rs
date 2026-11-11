/**
 * Tab ids for the admin console — shared by the server `page` (which
 * fetches per-tab data) and the client `admin-console` (tab buttons).
 * Kept in a plain module: a server component cannot read runtime
 * values out of a `"use client"` file.
 */
export const ADMIN_TABS = [
  "users",
  "roles",
  "permissions",
  "access",
  "vault",
  "storage",
  "activity",
] as const;
export type AdminTab = (typeof ADMIN_TABS)[number];
