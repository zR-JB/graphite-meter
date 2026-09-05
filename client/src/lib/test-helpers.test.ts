/** Replace test globals and restore their original descriptors, including absent properties. */
export function stubGlobals(values: Record<string, unknown>): () => void {
  const previous = Object.entries(values).map(([key, value]) => {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, key);
    Object.defineProperty(globalThis, key, {
      value,
      configurable: true,
      writable: true,
    });
    return [key, descriptor] as const;
  });
  return () => {
    for (const [key, descriptor] of previous.toReversed()) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    }
  };
}
