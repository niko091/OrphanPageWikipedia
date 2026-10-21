// Local worker for the pinned upstream fastText 0.9.3 engine. Rust communicates
// through bounded request batches; no foreign pointers cross the boundary.
#include "fasttext.h"
#include <array>
#include <cstdint>
#include <exception>
#include <iomanip>
#include <iostream>
#include <limits>
#include <sstream>
#include <string>
#include <vector>

namespace {
void write_u32(std::uint32_t value) {
  std::array<char, 4> bytes{};
  for (unsigned i = 0; i < bytes.size(); ++i)
    bytes[i] = static_cast<char>((value >> (8 * i)) & 255);
  std::cout.write(bytes.data(), bytes.size());
}
std::uint32_t read_u32() {
  std::array<unsigned char, 4> bytes{};
  for (auto &byte : bytes) {
    const auto next = std::cin.get();
    if (next == std::char_traits<char>::eof())
      throw std::runtime_error("Incomplete fastText request");
    byte = static_cast<unsigned char>(next);
  }
  std::uint32_t value = 0;
  for (unsigned i = 0; i < bytes.size(); ++i)
    value |= std::uint32_t(bytes[i]) << (8 * i);
  return value;
}
void write_string(const std::string &value) {
  const auto size = std::uint64_t(value.size());
  write_u32(static_cast<std::uint32_t>(size));
  write_u32(static_cast<std::uint32_t>(size >> 32));
  std::cout.write(value.data(), static_cast<std::streamsize>(value.size()));
}
std::string read_string() {
  const auto low = read_u32();
  const auto high = read_u32();
  const auto size = std::uint64_t(low) | (std::uint64_t(high) << 32);
  if (size > std::uint64_t(std::numeric_limits<std::streamsize>::max()))
    throw std::runtime_error("fastText request is too large for this platform");
  std::string value(static_cast<std::size_t>(size), '\0');
  std::cin.read(value.data(), static_cast<std::streamsize>(value.size()));
  if (!std::cin || value.find('\0') != std::string::npos ||
      value.find('\n') != std::string::npos)
    throw std::runtime_error("Invalid fastText input line");
  return value;
}
std::vector<std::string> read_batch(std::uint32_t maximum) {
  const auto count = read_u32();
  if (count > maximum)
    throw std::runtime_error("Invalid fastText batch size");
  std::vector<std::string> inputs;
  inputs.reserve(count);
  for (std::uint32_t i = 0; i < count; ++i)
    inputs.push_back(read_string());
  return inputs;
}
}

int main(int argc, char **argv) {
  std::ios::sync_with_stdio(false);
  std::cin.tie(nullptr);
  try {
    if (argc != 2)
      throw std::runtime_error("Expected one verified model path");
    fasttext::FastText model;
    model.loadModel(argv[1]);
    if (model.getArgs().model != fasttext::model_name::sup)
      throw std::runtime_error("Expected a supervised fastText model");
    const auto dictionary = model.getDictionary();
    if (dictionary->nlabels() != 64)
      throw std::runtime_error("Expected all 64 approved topic labels");
    write_u32(0);
    write_u32(static_cast<std::uint32_t>(dictionary->nlabels()));
    for (std::int32_t i = 0; i < dictionary->nlabels(); ++i)
      write_string(dictionary->getLabel(i));
    std::cout.flush();
    while (true) {
      const auto operation = std::cin.get();
      if (operation == std::char_traits<char>::eof())
        return 0;
      if (operation == 1) {
        const auto words = read_batch(4096);
        write_u32(0);
        write_u32(static_cast<std::uint32_t>(words.size()));
        for (const auto &word : words)
          write_u32(static_cast<std::uint32_t>(model.getWordId(word)));
      } else if (operation == 2) {
        const auto inputs = read_batch(10000);
        write_u32(0);
        write_u32(static_cast<std::uint32_t>(inputs.size()));
        for (const auto &input : inputs) {
          // The frozen Python batch adapter appends one newline per input too.
          std::istringstream stream(input + "\n");
          std::vector<std::pair<fasttext::real, std::string>> predictions;
          model.predictLine(stream, predictions, -1, 0.0);
          if (predictions.size() > 64)
            throw std::runtime_error("Unexpected fastText prediction size");
          write_u32(0);
          write_u32(static_cast<std::uint32_t>(predictions.size()));
          for (const auto &prediction : predictions) {
            write_string(prediction.second);
            std::ostringstream probability;
            probability << std::setprecision(std::numeric_limits<fasttext::real>::max_digits10)
                        << prediction.first;
            write_string(probability.str());
          }
        }
      } else {
        throw std::runtime_error("Unknown fastText operation");
      }
      std::cout.flush();
      if (!std::cout)
        return 1;
    }
  } catch (const std::exception &error) {
    write_u32(1);
    write_string(error.what());
    std::cout.flush();
    return 1;
  }
}
