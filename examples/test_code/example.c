#include <stdio.h>

void greet(const char* name) {
    printf("Hello, %s!\n", name);
}

int add(int a, int b) {
    return a + b;
}

int main() {
    greet("C");
    int result = add(5, 3);
    printf("5 + 3 = %d\n", result);
    return 0;
}
