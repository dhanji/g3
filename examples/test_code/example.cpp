#include <iostream>
#include <string>

class Person {
private:
    std::string name;
    int age;
    
public:
    Person(const std::string& name, int age) : name(name), age(age) {}
    
    void greet() {
        std::cout << "Hello, I'm " << name << " and I'm " << age << " years old." << std::endl;
    }
};

int main() {
    Person person("Alice", 30);
    person.greet();
    return 0;
}
