/** Run `fn` over `items` with at most `n` tasks in flight (unordered). */
export async function pool<T>(
  items: readonly T[],
  n: number,
  fn: (item: T, index: number) => Promise<void>
): Promise<void> {
  const st = { next: 0 };
  async function worker() {
    while (st.next < items.length) {
      const i = st.next++;
      await fn(items[i], i);
    }
  }
  await Promise.all(Array.from({ length: Math.min(n, items.length) }, worker));
}
