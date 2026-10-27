let nativeCalculate: (kind: string, input: string) => string;
export function calculateReport<T>(kind: string, input: unknown): T {
  return JSON.parse(nativeCalculate(kind, JSON.stringify(input))) as T;
}
export function setCalculator(calculate: (kind: string, input: string) => string): void {nativeCalculate=calculate;}
