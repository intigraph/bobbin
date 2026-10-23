#!/usr/bin/env python3
"""Private corpus controls for Bobbin's copied pointer-only release query.

# arming: agent-invoked; run before publishing the repository-pendant source.
"""
from pathlib import Path
import unittest
from rdflib import Dataset, Graph, Literal, Namespace, RDF, RDFS, URIRef

A = Namespace('http://aegis.gastown.local/ontology/')
CODE = Namespace(str(A) + 'code/bobbin/')
DOC = Namespace(str(A) + 'doc/bobbin/')
QUERY = (Path(__file__).resolve().parents[1] / 'queries/repository-share-bobbin.rq').read_text()


class RepositoryQuery(unittest.TestCase):
    def test_pointer_corpus_and_private_boundaries(self):
        source = Graph()
        doc, chunk = DOC['README.md'], URIRef(str(A) + 'chunk/bobbin/README.md#C1')
        expected = {
            (doc, RDF.type, A.Document),
            (doc, A.filePath, Literal('README.md')),
            (chunk, RDF.type, A.Chunk),
            (chunk, A.inDocument, doc),
            (chunk, A.chunkOrder, Literal(0)),
            (chunk, A.mentions, Literal('Store')),
            (CODE['store.rs'], A.filePath, Literal('src/store.rs')),
        }
        for triple in expected:
            source.add(triple)
        blocked = {
            (chunk, A.content, Literal('body must not be exported')),
            (chunk, A.startLine, Literal(1)),
            (chunk, A.endLine, Literal(30)),
            (chunk, A.mentions, URIRef('https://example.org/resolved')),
            (DOC['private'], RDFS.label, Literal('aegis-private123')),
            (DOC['private'], RDF.type, A.Document),
            (URIRef(str(A) + 'doc/other/README.md'), A.filePath, Literal('foreign')),
            (URIRef('urn:quipu:memory:private'), RDFS.label, Literal('memory')),
        }
        for triple in blocked:
            source.add(triple)
        result = set(source.query(QUERY).graph)
        self.assertEqual(result, expected)

    def test_ideas_and_sssom_records_survive_without_identity_knots(self):
        source = Graph()
        S = Namespace('https://w3id.org/sssom/')
        OWL = Namespace('http://www.w3.org/2002/07/owl#')
        record = URIRef('https://example.org/fixture/mapping')
        pointer = DOC['guide.md#idea']
        other = URIRef('https://example.org/other/idea')
        expected = {
            (pointer, RDF.type, A.Concept),
            (pointer, RDFS.comment, Literal('Curated high-level idea')),
            (pointer, A.reviewAfter, Literal('2026-11-01T00:00:00Z')),
            (record, RDF.type, OWL.Axiom),
            (record, S.subject_id, pointer),
            (record, S.object_id, other),
            (record, S.predicate_id, URIRef('http://www.w3.org/2004/02/skos/core#closeMatch')),
            (record, S.mapping_justification, URIRef('https://w3id.org/semapv/vocab/LexicalMatching')),
        }
        for triple in expected: source.add(triple)
        source.add((pointer, OWL.sameAs, other))
        self.assertEqual(set(source.query(QUERY).graph), expected)

    def test_public_policy_catalogue_survives_and_private_policy_does_not(self):
        POLICY = 'https://quipu.dev/knowledge/publication-policy/'
        public = Graph()
        for name in ('access-tokens', 'private-keys', 'user-directories', 'demo-private-domain'):
            rule = URIRef(POLICY + name)
            public.add((rule, RDF.type, A.InternalIdentifierPattern))
            public.add((rule, RDFS.label, Literal(name + ' label')))
            public.add((rule, A.regex, Literal(name + '-pattern')))
            public.add((rule, A.enforcementTier, Literal('block')))
        rules = set(public.subjects(RDF.type, A.InternalIdentifierPattern))
        source = Dataset(default_union=True)
        for triple in public:
            source.add(triple)
        shadowed = URIRef(POLICY + 'private-keys')
        private = source.graph(URIRef('urn:quipu:private-release-policy'))
        # A private rule, and a private regex attached to a public rule IRI.
        private.add((URIRef(POLICY + 'internal-hosts'), RDF.type, A.InternalIdentifierPattern))
        private.add((URIRef(POLICY + 'internal-hosts'), A.regex, Literal('corp[.]internal')))
        private.add((shadowed, A.regex, Literal('secret-host-name')))
        # A public rule IRI cannot carry an unreviewed predicate.
        source.add((URIRef(POLICY + 'access-tokens'), RDFS.comment, Literal('extra')))
        result = set(source.query(QUERY).graph)
        expected = {
            t for t in public
            if t[0] != shadowed and t[1] in (RDF.type, RDFS.label, A.regex, A.enforcementTier)
        }
        self.assertEqual(result, expected)
        self.assertEqual(
            {s for s, _, _ in result}, rules - {shadowed}
        )
        self.assertFalse(any('secret-host-name' in str(o) or 'corp' in str(o) for _, _, o in result))

    def test_empty_is_empty_with_populated_positive(self):
        self.assertEqual(len(Graph().query(QUERY).graph), 0)
        source = Graph()
        triple = (DOC['README.md'], A.filePath, Literal('README.md'))
        source.add(triple)
        self.assertEqual(set(source.query(QUERY).graph), {triple})


if __name__ == '__main__':
    unittest.main()
