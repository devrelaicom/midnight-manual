import type {ReactNode} from 'react';
import Layout from '@theme/Layout';
import HomeSections from '@site/src/components/home/HomeSections';

export default function Home(): ReactNode {
  return (
    <Layout
      title="Ask your docs, not your model"
      description="Private, current search over the real Midnight docs and source — right inside your AI assistant.">
      <main><HomeSections /></main>
    </Layout>
  );
}
