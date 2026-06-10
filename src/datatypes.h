//
// Created by Willow on 6/10/26.
//

#ifndef TETRA_DATATYPES_H
#define TETRA_DATATYPES_H
#include <string>

struct keyvalue_pair {
    std::string key;
    std::string value;
    bool editable;
    bool required;
};

#endif // TETRA_DATATYPES_H
