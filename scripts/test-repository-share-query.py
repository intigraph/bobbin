#!/usr/bin/env python3
"""Private corpus controls for Bobbin's copied pointer-only release query.

# arming: agent-invoked; run before publishing the repository-pendant source.
"""
from pathlib import Path
import unittest
from rdflib import Graph, Literal, Namespace, RDF, RDFS, URIRef

A = Namespace('http://aegis.gastown.local/ontology/')
CODE = Namespace(str(A) + 'code/bobbin/')
DOC = Namespace(str(A) + 'doc/bobbin/')
QUERY = (Path(__file__).resolve().parents[1] / 'queries/repository-share-bobbin.rq').read_text()


class RepositoryQuery(unittest.TestCase):
    def test_pointer_corpus_and_private_boundaries(self):
        source = Graph()
        doc, chunk = DOC['README.md'], DOC['README.md/chunk/0']
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

    def test_empty_is_empty_with_populated_positive(self):
        self.assertEqual(len(Graph().query(QUERY).graph), 0)
        source = Graph()
        triple = (DOC['README.md'], A.filePath, Literal('README.md'))
        source.add(triple)
        self.assertEqual(set(source.query(QUERY).graph), {triple})


if __name__ == '__main__':
    unittest.main()
