//
// Created by Willow on 6/10/26.
//

#ifndef TETRA_DATATYPES_H
#define TETRA_DATATYPES_H
#include <string>

struct recipeKeyValuePair {
    std::string key;
    std::string value;
    bool editable;
    bool required;
};

struct recipeValueOnly {
    std::string value;
    bool editable;
    bool required;
};

struct userKeyValuePair {
    std::string key;
    std::string value;
};

struct userValueOnly {
    std::string value;
};

#endif // TETRA_DATATYPES_H
