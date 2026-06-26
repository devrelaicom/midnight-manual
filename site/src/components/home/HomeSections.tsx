import type {ReactNode} from 'react';
import Hero from './sections/Hero';
import Why from './sections/Why';
import TrustTrio from './sections/TrustTrio';
import GetStarted from './sections/GetStarted';
import ThreeWays from './sections/ThreeWays';
import Skill from './sections/Skill';
import Privacy from './sections/Privacy';

export default function HomeSections(): ReactNode {
  return (
    <>
      <Hero /><Why /><TrustTrio /><GetStarted /><ThreeWays /><Skill /><Privacy />
    </>
  );
}
