// Narrow C ABI for the pinned upstream fastText 0.9.3 engine. No Python API.
#include "fasttext.h"
#include <cstdio>
#include <exception>
#include <memory>
#include <sstream>

namespace {
template <typename F> int checked(char *error, size_t capacity, F operation) noexcept {
  try {
    operation();
    return 0;
  } catch (const std::exception &e) {
    std::snprintf(error, capacity, "%s", e.what());
  } catch (...) {
    std::snprintf(error, capacity, "%s", "Unknown fastText error");
  }
  return -1;
}
}

extern "C" {
using TopicCallback = void (*)(void *, const char *, size_t, float);

int ow_topic_load(const char *path, void **handle, char *error, size_t capacity) noexcept {
  return checked(error, capacity, [&] {
    auto model = std::make_unique<fasttext::FastText>();
    model->loadModel(path);
    if (model->getArgs().model != fasttext::model_name::sup)
      throw std::invalid_argument("Expected a supervised fastText model");
    *handle = model.release();
  });
}

void ow_topic_free(void *handle) noexcept {
  delete static_cast<fasttext::FastText *>(handle);
}

int ow_topic_labels(void *handle, void *context, TopicCallback receive,
                    char *error, size_t capacity) noexcept {
  return checked(error, capacity, [&] {
    const auto dictionary = static_cast<fasttext::FastText *>(handle)->getDictionary();
    for (int32_t i = 0; i < dictionary->nlabels(); ++i) {
      const auto label = dictionary->getLabel(i);
      receive(context, label.data(), label.size(), 0);
    }
  });
}

int ow_topic_word(void *handle, const char *word) noexcept {
  return static_cast<fasttext::FastText *>(handle)->getWordId(word);
}

int ow_topic_predict(void *handle, const char *text, void *context,
                     TopicCallback receive, char *error, size_t capacity) noexcept {
  return checked(error, capacity, [&] {
    // The Python batch adapter appends exactly one newline per input too.
    std::istringstream stream(std::string(text) + "\n");
    std::vector<std::pair<fasttext::real, std::string>> predictions;
    static_cast<fasttext::FastText *>(handle)->predictLine(stream, predictions, -1, 0.0);
    for (const auto &prediction : predictions)
      receive(context, prediction.second.data(), prediction.second.size(), prediction.first);
  });
}
}
