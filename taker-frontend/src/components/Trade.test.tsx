import { isEvenlyDivisible } from "./Trade";

it("1200 is evenly divisible by 100.0", () => {
    expect(isEvenlyDivisible(1200, 100.0)).toBe(true);
});

it("233.3 is not evenly divisible by 100.0", () => {
    expect(isEvenlyDivisible(233.3, 100.0)).toBe(false);
});
