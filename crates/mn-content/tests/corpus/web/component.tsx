/**
 * A simple React component for the corpus fixture.
 * This file exercises TypeScript TSX chunking.
 */

/** Props for the Greeting component. */
interface GreetingProps {
  name: string;
  count?: number;
}

/** Renders a greeting message with an optional count. */
export function Greeting({ name, count = 0 }: GreetingProps): JSX.Element {
  return (
    <div className="greeting">
      <h1>Hello, {name}!</h1>
      {count > 0 && <p>You have visited {count} times.</p>}
    </div>
  );
}

/** A counter display component. */
export function Counter({ value }: { value: number }): JSX.Element {
  return (
    <span className="counter" data-value={value}>
      {value}
    </span>
  );
}

/** Default export: the main page component. */
export default function Page(): JSX.Element {
  return (
    <main>
      <Greeting name="Midnight" count={3} />
      <Counter value={42} />
    </main>
  );
}
