"""Phrase-aware statistical scorer for the LLM-free email demo.

The model and hyperparameters were selected using the original development
split only. No held-out or external corpus is fitted by this module.
"""
import numpy as np
from sklearn.feature_extraction.text import TfidfVectorizer
from sklearn.linear_model import LogisticRegression


class PhraseScorer:
    def __init__(self, rows, legacy_class):
        self.legacy = legacy_class(rows)
        self.vectorizer = TfidfVectorizer(
            ngram_range=(1, 2), sublinear_tf=True, min_df=2, max_features=100000,
        )
        matrix = self.vectorizer.fit_transform([r[0] for r in rows])
        self.classifier = LogisticRegression(
            C=4, solver='liblinear', max_iter=1000, random_state=7,
        ).fit(matrix, [r[1] for r in rows])
        self.tokenize = self.vectorizer.build_tokenizer()

    def encode_batch(self, texts):
        scores = self.classifier.decision_function(self.vectorizer.transform(texts))
        encoded = []
        for text, score in zip(texts, scores):
            old_x, old_prediction = self.legacy.encode(text)
            margin = float(np.tanh(score / 4.))
            prediction = int(score > 0.)
            x = [1., margin, margin * margin, margin ** 3, old_x[1], *old_x[4:]]
            # Review uncertainty rather than silently counting it as success.
            # The guard uses only message features and model disagreement.
            review = (prediction != old_prediction or len(self.tokenize(text)) < 5
                      or old_x[5] > 0.5)
            encoded.append((x, prediction, review))
        return encoded
